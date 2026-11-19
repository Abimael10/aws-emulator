Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-agent-runtime-not-found-errors
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-agent-runtime-not-found-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteAgentRuntimeEndpoint|4xx->4xx|$.message
Issue-Commit: b235f43ec24f1e947704718b25995d7af0836105

# bedrock-agentcore-control DeleteAgentRuntimeEndpoint not-found message names the endpoint instead of the missing parent agent runtime

> _Note: this issue was identified against the code base at commit `b235f43ec24f1e947704718b25995d7af0836105`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAgentRuntimeEndpoint

## Description
Cause 1 of 1: DeleteAgentRuntimeEndpoint on an agent runtime that does not exist returns ResourceNotFoundException with the wrong message — it describes a missing endpoint rather than the missing parent runtime.

AWS: DeleteAgentRuntimeEndpoint (Smithy: DELETE /runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/, success 202, modeled errors AccessDeniedException 403, ConflictException 409, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500) resolves its parent resource FIRST. When `agentRuntimeId` names no runtime, AWS never gets as far as looking up `endpointName`: it answers HTTP 404 with x-amzn-errortype ResourceNotFoundException and the agent-scoped message. The recorded response shows that message as "Agent '<agentRuntimeId>' was not found. Please check the agent ID and try again." — the fixed text is "Agent '" ... "' was not found. Please check the agent ID and try again.", and the only variable part is the `agentRuntimeId` path label taken verbatim from the request URI (in this recording the value 'missingruntime-abcdefghij'; that value is an example, not part of the template). The endpoint name does not appear in it at all. The model documents no message text, so the exact wording is recorded behavior only; the status (404) and error code (ResourceNotFoundException) are modeled and also match the recording.

Emulator: the status and error code are correct (404, ResourceNotFoundException), but the message is "Agent endpoint with name <endpointName> not found for agent <agentRuntimeId>" — in this replay 'Agent endpoint with name missingendpoint not found for agent missingruntime-abcdefghij'. That is an endpoint-scoped message built from both path labels, i.e. the handler skips the parent-runtime existence check and falls through to the per-endpoint lookup miss, reporting the child as the missing resource.

Why: a client cannot tell from the emulator which resource is actually absent. Code that distinguishes "the runtime is gone, stop retrying / re-create it" from "the runtime is fine, only this endpoint is missing / create the endpoint" branches on the message text, because both cases share the same 404 and the same error code. Against the emulator such a client takes the endpoint branch and re-attempts endpoint operations on a runtime that will never exist; log scrapers and test assertions matching AWS's "was not found. Please check the agent ID" wording also fail.

Fix: the DeleteAgentRuntimeEndpoint handler for bedrock-agentcore-control — it must look up `agentRuntimeId` before `endpointName` and, when the runtime is absent, raise ResourceNotFoundException with AWS's agent-scoped message, interpolating the request's `agentRuntimeId`; only when the runtime exists and the endpoint does not should it emit the endpoint-scoped message.

Traces: fixture bedrock-agentcore-control-agent-runtime-not-found-errors, step task#3 (cluster of 1).

## Evidence
Replay of fixture bedrock-agentcore-control-agent-runtime-not-found-errors, step task#3, operation DeleteAgentRuntimeEndpoint — verdict mismatch, affected_count 1.

Status: expected 404, actual 404 (matches).
Error type header: ResourceNotFoundException (matches).
$.message: value_mismatch
  expected: "Agent 'missingruntime-abcdefghij' was not found. Please check the agent ID and try again."
  actual:   "Agent endpoint with name missingendpoint not found for agent missingruntime-abcdefghij"

divergence_key: service=bedrock-agentcore-control|DeleteAgentRuntimeEndpoint|status=404->404|keys=$.message

Template breakdown of the AWS message: fixed text "Agent '" + <agentRuntimeId from request path> + "' was not found. Please check the agent ID and try again." No endpoint name, no account id, no request id, no timestamp — nothing per-run.

Smithy evidence (com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json): DeleteAgentRuntimeEndpoint input agentRuntimeId (httpLabel, required, pattern ^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$) and endpointName (httpLabel, required, pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}$); errors include ResourceNotFoundException httpError 404.

No open issue covers this: the only DeleteAgentRuntimeEndpoint/GetAgentRuntime issues on file are 20261115T032640Z-bedrock-agentcore-control-getagentruntime-answers-validationexception-where-aws.md (GetAgentRuntime returning ValidationException instead of AccessDeniedException) and 20261029T212239Z-bedrock-agentcore-control-accepts-four-requests-aws-refuses.md (requests wrongly accepted); neither addresses this message text. The replay reported owned_by: [] and stub_path: "" for this cluster.

## Steps to Reproduce
1. Boot the emulator from this checkout (bedrock-agentcore-control is bootstrapped; get_bootstrap_status reports no pending services).
2. Replay fixture bedrock-agentcore-control-agent-runtime-not-found-errors and stop at step task#3, or issue the call directly:
     DELETE /runtimes/missingruntime-abcdefghij/runtime-endpoints/missingendpoint/
   against the bedrock-agentcore-control endpoint, with no runtime of that id ever created.
3. Observe HTTP 404 with x-amzn-errortype: ResourceNotFoundException and body message "Agent endpoint with name missingendpoint not found for agent missingruntime-abcdefghij".
4. AWS answers the same 404/ResourceNotFoundException but with "Agent 'missingruntime-abcdefghij' was not found. Please check the agent ID and try again." — i.e. the parent runtime is reported missing, not the endpoint.
5. Contrast: create a runtime, then delete a non-existent endpoint on it; only that case should produce an endpoint-scoped message.
