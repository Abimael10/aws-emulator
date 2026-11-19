Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-runtime-flow-execution-not-found
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-runtime-flow-execution-not-found bedrock-agent-runtime-start-execution-then-inspect-unknown-flow
Issue-Shape-Id: com.amazonaws.bedrockagentruntime#AmazonBedrockAgentRunTimeService
Issue-Key: bedrock-agent-runtime|StartFlowExecution|4xx->4xx|$.message
Issue-Commit: b235f43ec24f1e947704718b25995d7af0836105

# bedrock-agent-runtime: StartFlowExecution ResourceNotFoundException carries the generic Bedrock not-found message instead of the typed 'Resource not found, type: Flow' text

> _Note: this issue was identified against the code base at commit `b235f43ec24f1e947704718b25995d7af0836105`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartFlowExecution

## Description
Cause 1 of 1: StartFlowExecution against a flow identifier that does not exist returns the right HTTP status and error code, but the WRONG error message.

AWS: Smithy model services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json — StartFlowExecution POST /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions, errors include ResourceNotFoundException (client, httpError 404), documented as "The specified resource Amazon Resource Name (ARN) was not found. Check the Amazon Resource Name (ARN) and try your request again." The model does not fix the message string, so the message requirement is recorded behavior only: the recording shows the agent-runtime flow surface answering 404 ResourceNotFoundException with the message 'Resource not found, type: Flow'. That message is a template: the fixed text is "Resource not found, type: " and the variable part is the type of resource the lookup failed on, taken from the request path being resolved — here the flow addressed by flowIdentifier, hence "Flow".

Emulator: answers the same 404 and the same x-amzn-errortype, but the message body is the generic Bedrock not-found sentence "Failed to retrieve resource because it doesn't exist. Retry the request with a different resource identifier." — i.e. the service-wide default not-found text, with no resource-type discriminator at all.

Why: a client that distinguishes which resource in the request was missing (flow vs alias vs execution) can only do so from the message, because the status (404) and the error code (ResourceNotFoundException) are identical for every one of them. Callers and assertions keyed on the typed text — and users reading the error — are told nothing about what was not found, and tests that match AWS's message fail against the emulator.

Fix: the StartFlowExecution handler's not-found path in the bedrock-agent-runtime service crate (the flow-execution resource lookup / its ResourceNotFoundException construction) must build the message from the resource type it failed to resolve rather than falling through to the crate's shared generic not-found string; the same typed form must be used for the unknown-flow lookup reached by both recorded fixtures.

Traces: bedrock-agent-runtime-flow-execution-not-found#task#2 (representative) and bedrock-agent-runtime-start-execution-then-inspect-unknown-flow#task#1 — both unknown-flow StartFlowExecution calls, same cluster.

No open issue covers this: the nearest bedrock-agent-runtime issues are 20261111T002352Z (GetDocumentContent userContext/ValidationException) and 20261114T225754Z (Rerank relevanceScore scale), neither of which touches the flow-execution not-found message.

## Evidence
Replay cluster service=bedrock-agent-runtime|StartFlowExecution|status=404->404|keys=$.message, verdict=mismatch, affected_count=2.
Expected status 404, actual status 404 (status matches).
$.message: value_mismatch — expected 'Resource not found, type: Flow', got "Failed to retrieve resource because it doesn't exist. Retry the request with a different resource identifier."
Error code / x-amzn-errortype matched (ResourceNotFoundException); only the message text diverges.
Fixed text of the required message: "Resource not found, type: "; variable part: the resource type being resolved from the request ("Flow" for a flowIdentifier lookup).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Replay fixture bedrock-agent-runtime-flow-execution-not-found, step task#2: POST /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions with a syntactically valid but nonexistent flowIdentifier (StartFlowExecution).
3. Observe HTTP 404 with x-amzn-errortype ResourceNotFoundException but body message "Failed to retrieve resource because it doesn't exist. Retry the request with a different resource identifier." instead of 'Resource not found, type: Flow'.
4. The same divergence reproduces on fixture bedrock-agent-runtime-start-execution-then-inspect-unknown-flow, step task#1.
