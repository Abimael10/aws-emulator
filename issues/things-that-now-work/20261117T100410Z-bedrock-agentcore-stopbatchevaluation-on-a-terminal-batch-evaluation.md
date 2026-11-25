Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.stop-completed-batch-evaluation-conflict
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.stop-completed-batch-evaluation-conflict|run#17|AssertionError|assert message == (
Issue-Commit: 6467559f5313a198a0c73115b08bb15d3259e538

# bedrock-agentcore StopBatchEvaluation on a terminal batch evaluation answers ConflictException with emulator-authored status prose instead of AWS's fixed concurrent-modification sentence

> _Note: this issue was identified against the code base at commit `6467559f5313a198a0c73115b08bb15d3259e538`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopBatchEvaluation

## Description
Cause 1 of 1: StopBatchEvaluation's ConflictException message is written by the emulator's batch-evaluation state module rather than reproducing AWS's fixed ConflictException wording, so the HTTP status (409) and the error code (ConflictException) are right but the message body is not AWS's.

Failing check ids: bedrock-agentcore.stop-completed-batch-evaluation-conflict

AWS: Smithy (services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json): StopBatchEvaluation has http POST /evaluations/batch-evaluate/{batchEvaluationId}/stop with success code 202, input member batchEvaluationId (@required, @httpLabel, pattern ^[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$), and models ConflictException (httpError 409) — documented as "the request conflicts with the current state of the resource ... when trying to modify a resource that is currently being modified by another request". The model documents the error class but not its message text, so the message requirement is recorded behavior only: the recording shows that stopping a batch evaluation that has already reached a terminal status is refused 409 ConflictException whose Message is AWS's fixed, wholly invariant sentence for this error class — "Failed to update the resource as a concurrent request modified the resource. Retry the request after the other request has completed." Every word of that message is fixed text; it interpolates nothing from the request or from state (no batchEvaluationId, no status name, no limit), so there is no variable part to fill from a particular run.

Emulator: the same call is refused 409 ConflictException, but the Message is emulator-authored prose that names the job's current status and enumerates the stoppable ones: 'Cannot stop batch evaluation in status STOPPED. Only PENDING or IN_PROGRESS evaluations can be stopped.' (the status token in that sentence is whichever terminal status the job happens to hold — STOPPED in the replay — which is itself evidence the emulator is templating a message AWS does not template). It comes from crate::state::batch_evaluation_not_stoppable_message(status), passed into ConflictException at services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/stop_batch_evaluation.rs lines 214-219. The rest of the operation is correct: the first stop on a non-terminal job is accepted, the job settles to a terminal status, and the rejected second stop leaves the status unchanged — only the refusal's message text diverges.

Why: a client that keys off the refusal's message — log/alarm matchers, test assertions, retry-classification code, or any user-facing surface that relays AWS's sentence — sees text AWS never sends. AWS's sentence instructs the caller to retry after the concurrent request completes; the emulator's instead asserts a status precondition, so a client following the emulator's text draws the opposite operational conclusion about whether the call is retryable, and message-equality assertions against the real service fail.

Fix: the ConflictException construction in StopBatchEvaluation's handler and the message helper it calls in this service's batch-evaluation state module — the not-stoppable refusal must carry AWS's fixed concurrent-modification sentence verbatim rather than a status-interpolated sentence of the emulator's own. The status gate itself (which statuses are stoppable) is correct and should not change.

Traces: bedrock-agentcore.stop-completed-batch-evaluation-conflict (mismatch; assertion at check_bedrock_agentcore_stop_completed_batch_evaluation_conflict.py line 59 — "expected the exact conflict message, got 'Cannot stop batch evaluation in status STOPPED. Only PENDING or IN_PROGRESS evaluations can be stopped.'"). Code citations: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/stop_batch_evaluation.rs:209-220 (gate and ConflictException), :216 (crate::state::batch_evaluation_not_stoppable_message).

## Evidence
Check: bedrock-agentcore.stop-completed-batch-evaluation-conflict [mismatch]

Request sequence: CreateLogGroup (logs) -> StartBatchEvaluation(batchEvaluationName=<alnum name>, dataSourceConfig.cloudWatchLogs{serviceNames, logGroupNames}, evaluators=[{evaluatorId: Builtin.GoalSuccessRate}]) -> StopBatchEvaluation(batchEvaluationId) [accepted] -> poll GetBatchEvaluation until terminal -> StopBatchEvaluation(batchEvaluationId) again.

Expected (AWS, from the recording):
  HTTP 409
  Error.Code    = "ConflictException"
  Error.Message = "Failed to update the resource as a concurrent request modified the resource. Retry the request after the other request has completed."
  (entirely fixed text — no request or state values are interpolated)

Actual (emulator):
  HTTP 409
  Error.Code    = "ConflictException"   <- matches
  Error.Message = "Cannot stop batch evaluation in status STOPPED. Only PENDING or IN_PROGRESS evaluations can be stopped."   <- diverges
  (the 'STOPPED' token is the job's current status, i.e. a value templated from emulator state; AWS's message templates nothing)

Harness failure:
  File "check_bedrock_agentcore_stop_completed_batch_evaluation_conflict.py", line 59, in run
    assert message == (...)
  AssertionError: expected the exact conflict message, got 'Cannot stop batch evaluation in status STOPPED. Only PENDING or IN_PROGRESS evaluations can be stopped.'

Status is otherwise correct: GetBatchEvaluation after the rejected second stop reports the same terminal status, so no state was mutated by the refusal.

## Steps to Reproduce
1. Create a CloudWatch log group (StartBatchEvaluation validates that the referenced log group exists).
2. Call bedrock-agentcore StartBatchEvaluation with that log group in dataSourceConfig.cloudWatchLogs and one evaluator (e.g. evaluatorId Builtin.GoalSuccessRate); keep the returned batchEvaluationId.
3. Call StopBatchEvaluation(batchEvaluationId) once while the job is still PENDING/IN_PROGRESS — it is accepted (202).
4. Poll GetBatchEvaluation(batchEvaluationId) until status is terminal (COMPLETED | COMPLETED_WITH_ERRORS | FAILED | STOPPED).
5. Call StopBatchEvaluation(batchEvaluationId) a second time and inspect the error body.
   Observed: 409 ConflictException with Message 'Cannot stop batch evaluation in status <STATUS>. Only PENDING or IN_PROGRESS evaluations can be stopped.'
   Expected: 409 ConflictException with Message 'Failed to update the resource as a concurrent request modified the resource. Retry the request after the other request has completed.'
6. Confirm the refusal had no effect: GetBatchEvaluation still reports the same terminal status (this part already matches).
