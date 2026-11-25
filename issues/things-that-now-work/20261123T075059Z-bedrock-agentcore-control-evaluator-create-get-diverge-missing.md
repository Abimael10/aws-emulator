Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-evaluator-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-evaluator-error-paths bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|CreateEvaluator|4xx->2xx|$.fieldList
Issue-Key: bedrock-agentcore-control|CreateEvaluator|4xx->2xx|$.message
Issue-Key: bedrock-agentcore-control|CreateEvaluator|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|CreateEvaluator|4xx->2xx|status
Issue-Key: bedrock-agentcore-control|CreateEvaluator|2xx->2xx|$.status
Issue-Key: bedrock-agentcore-control|GetEvaluator|2xx->2xx|$.status
Issue-Commit: 35d7082131f9491ad89c2e4b5f0079c864ac9dfa

# bedrock-agentcore-control evaluator create/get diverge: missing instructions-placeholder validation and status stuck at CREATING (3 causes)

> _Note: this issue was identified against the code base at commit `35d7082131f9491ad89c2e4b5f0079c864ac9dfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEvaluator

## Description
Three net-new causes in the bedrock-agentcore-control custom-evaluator surface. They share one fault: the evaluator handler neither validates the llmAsAJudge instructions against the evaluator level nor ever settles the resource it creates, so every create is accepted and every read reports a provisional status. All three must be closed before submitting.

Cause 1 of 3: CreateEvaluator accepts an llmAsAJudge evaluator whose instructions contain none of the placeholders allowed for the requested level.
  AWS: CreateEvaluator (Smithy: POST /evaluators/create, success code 202; input evaluatorConfig (union EvaluatorConfig, required) + level (enum EvaluatorLevel, required: TOOL_CALL|TRACE|SESSION); modeled error ValidationException, errorKind client, httpError 400, members message (required), reason (required), fieldList). AWS validates that LlmAsAJudgeEvaluatorConfig.instructions — documented as "The evaluation instructions that guide the language model in assessing agent performance" — reference at least one placeholder from the set allowed for the submitted level, and rejects the request before any resource exists. The recorded response is HTTP 400 with header x-amzn-errortype: ValidationException, $.message "Instructions must contain at least one of the allowed placeholders for SESSION level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions", and the same text repeated in $.fieldList as a single entry {"name": "instructions", "message": "<that same text>"}. Message template: the fixed text is "Instructions must contain at least one of the allowed placeholders for " + <level> + " level: " + <comma-separated allowed placeholder names for that level>; the only variable parts are the level taken from the request's level member and the placeholder list that the service defines for that level (for SESSION: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions). The fieldList entry's name is the offending member name, "instructions".
  Emulator: answers HTTP 202 with a created evaluator — no x-amzn-errortype header, no $.message, no $.fieldList — i.e. the placeholder/level check is absent entirely.
  Why: a client that relies on AWS rejecting an unusable judge prompt gets a success instead. Its error path (catching ValidationException and reading fieldList to point at the "instructions" field) is never exercised, and it proceeds believing it owns a working SESSION-level evaluator that AWS would never have created, leaving a resource behind that must be cleaned up.
  Fix: CreateEvaluator's input validation — add the per-level allowed-placeholder check on EvaluatorConfig.llmAsAJudge.instructions, keyed off the request's level, and raise ValidationException (400, x-amzn-errortype: ValidationException) with the message above plus the matching single-entry fieldList, before any state is mutated.
  Traces: bedrock-agentcore-control-evaluator-error-paths#task#3.

Cause 2 of 3: CreateEvaluator reports status CREATING where AWS reports ACTIVE.
  AWS: CreateEvaluator output member status is required and targets enum EvaluatorStatus with values ACTIVE, CREATING, CREATE_FAILED, UPDATING, UPDATE_FAILED, DELETING. For a custom llmAsAJudge evaluator the recording shows the create response already carrying $.status "ACTIVE": creation of this resource is not a long-running provisioning step, so the terminal status is returned on the create itself (recorded behavior for the status value; the enum and requiredness are from the model).
  Emulator: returns $.status "CREATING" on the 202 create response.
  Why: a client that treats the create response as authoritative sees a non-terminal status and either rejects the resource as not ready or enters a wait/poll loop that AWS never requires; combined with Cause 3 that loop never terminates.
  Fix: the CreateEvaluator handler's state transition — a newly created custom evaluator must be stored in, and serialized with, its terminal ACTIVE status rather than a provisional CREATING one (or must actually advance out of CREATING).
  Traces: bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle#setup#1.

Cause 3 of 3: GetEvaluator reports status CREATING where AWS reports ACTIVE, so the evaluator never settles.
  AWS: GetEvaluator (Smithy: GET /evaluators/{evaluatorId}, code 200; output member status required, enum EvaluatorStatus). A read of an existing custom llmAsAJudge evaluator returns $.status "ACTIVE" in the recording — the resource is readable in its terminal state immediately after creation.
  Emulator: returns HTTP 200 but with $.status "CREATING" for the evaluator just created, i.e. the stored status is never advanced to a terminal value.
  Why: a client polling GetEvaluator until status leaves CREATING — the normal AWS pattern for this enum — spins forever and the workflow deadlocks or times out; any gate of the form "status == ACTIVE before use" is never satisfied, so a correctly written client can never use an evaluator this emulator created.
  Fix: the evaluator state model / GetEvaluator serializer — persist and report the evaluator's terminal ACTIVE status (same stored field as Cause 2; fixing the stored value fixes both the create and the read path).
  Traces: bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle#task#2.

No open issue covers these: the two open issues matching "evaluator" (20261121T212257Z-appsync-evaluatemappingtemplate-vtl-evaluator-and-response-envelope-diverge.md and 20261119T193246Z-config-lambda-sqs-2-net-new-causes-a--r2.md) are AppSync and Config findings and carry no replay key for these steps. The replay report lists no owner for these three clusters.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200). Per-cluster expected-vs-actual as reported:

Cause 1 — fixture bedrock-agentcore-control-evaluator-error-paths, step task#3, CreateEvaluator:
  status: expected 400, got 202
  $headers.x-amzn-errortype: expected 'ValidationException', got None (missing)
  $.message: expected 'Instructions must contain at least one of the allowed placeholders for SESSION level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions', got None (missing)
  $.fieldList: expected [{'name': 'instructions', 'message': 'Instructions must contain at least one of the allowed placeholders for SESSION level: available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions'}], got None (missing)
  divergence_key: service=bedrock-agentcore-control|CreateEvaluator|status=400->202|keys=$.fieldList,$.message,$headers.x-amzn-errortype,status

Cause 2 — fixture bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle, step setup#1, CreateEvaluator:
  status 202 == 202; $.status: expected 'ACTIVE', got 'CREATING'
  divergence_key: service=bedrock-agentcore-control|CreateEvaluator|status=202->202|keys=$.status

Cause 3 — fixture bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle, step task#2, GetEvaluator:
  status 200 == 200; $.status: expected 'ACTIVE', got 'CREATING'
  divergence_key: service=bedrock-agentcore-control|GetEvaluator|status=200->200|keys=$.status

Model grounding: Smithy CreateEvaluator -> http POST /evaluators/create code 202, errors include ValidationException (client, httpError 400); ValidationException members message (required), reason (required), fieldList. Smithy GetEvaluator -> http GET /evaluators/{evaluatorId} code 200, output.status required. Smithy enum EvaluatorStatus = [ACTIVE, CREATING, CREATE_FAILED, UPDATING, UPDATE_FAILED, DELETING]; enum EvaluatorLevel = [TOOL_CALL, TRACE, SESSION]; EvaluatorConfig is a union with member llmAsAJudge -> LlmAsAJudgeEvaluatorConfig.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture 'bedrock-agentcore-control-evaluator-error-paths' step 'task#3' — POST /evaluators/create with level=SESSION and an evaluatorConfig.llmAsAJudge whose instructions string contains none of the SESSION-level placeholders (available_tools, context, actual_tool_trajectory, expected_tool_trajectory, assertions). Expect 400 + x-amzn-errortype: ValidationException + the placeholder message in $.message and in a one-entry $.fieldList naming field 'instructions'; observe 202 with a created evaluator and none of those fields.
4. Cause 2: replay fixture 'bedrock-agentcore-control-evaluator-llm-as-judge-lifecycle' step 'setup#1' — POST /evaluators/create for a valid llmAsAJudge evaluator. Expect $.status 'ACTIVE' on the 202 response; observe 'CREATING'.
5. Cause 3: replay the same fixture's step 'task#2' — GET /evaluators/{evaluatorId} for the evaluator created in step 4. Expect $.status 'ACTIVE' on the 200 response; observe 'CREATING', and note that repeating the GET never advances the status.
