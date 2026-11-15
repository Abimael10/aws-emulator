Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-flow-prepare-and-version
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-flow-prepare-and-version
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|GetFlowVersion|4xx->2xx|$.message
Issue-Key: bedrock-agent|GetFlowVersion|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agent|GetFlowVersion|4xx->2xx|status
Issue-Commit: 89c70456cbc5c776860ae07cdd8fb6580c26b557

# bedrock-agent GetFlowVersion still serves a flow version that no longer exists with 200 instead of 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `89c70456cbc5c776860ae07cdd8fb6580c26b557`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFlowVersion

## Description
1 cause.

Cause 1 of 1: a read of a numbered flow version the service no longer holds answers HTTP 200 with the version's full representation instead of the modeled 404 ResourceNotFoundException (fixture bedrock-agent-flow-prepare-and-version, step task#9).

AWS: Smithy (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/smithy-model/com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda.json) models GetFlowVersion as GET /flows/{flowIdentifier}/versions/{flowVersion}/ with success status 200, input flowIdentifier (FlowIdentifier, @required, @httpLabel) and flowVersion (NumericalVersion @pattern ^[0-9]{1,5}$, @required, @httpLabel), and it models ResourceNotFoundException (@error client, @httpError 404, documented "The specified resource Amazon Resource Name (ARN) was not found. Check the Amazon Resource Name (ARN) and try your request again."). DeleteFlowVersion is modeled DELETE /flows/{flowIdentifier}/versions/{flowVersion}/ with success 200 returning id+version, i.e. a numbered version is a deletable resource. Recorded behavior for this step: once the addressed flow holds no version under the requested NumericalVersion — while the flow ITSELF still resolves — the read is refused HTTP 404 ResourceNotFoundException with the message "Couldn't perform GetFlowVersion operation. Failed to retrieve resource type FlowVersion because it doesn't exist. Check the resource ID and retry your request." Fixed text of that template: "Couldn't perform " + <operation name> + " operation. Failed to retrieve resource type " + <resource type> + " because it doesn't exist. Check the resource ID and retry your request."; its only variable parts are the invoked operation name (here GetFlowVersion) and the type of the resource that failed to resolve (here FlowVersion, NOT Flow — the flow resolves, the version does not). No identifier appears in the message, and the response carries the x-amzn-errortype header for that error shape. The requirement: a numbered version that is gone is unreadable, and the refusal names FlowVersion.

Emulator: replay observed HTTP 200 where 404 was required, so no error body was produced at all — $.message is absent (expected the FlowVersion not-found sentence above) and the $headers.x-amzn-errortype error-type header is absent. The emulator served a GetFlowVersionResponse for a version AWS had already stopped serving. The read path itself is written correctly: services/.../src/ops/get_flow_version.rs lines 212-227 refuses a missing flow with flow_not_found_message and a missing version with flow_version_not_found_message via state.flow_version_of, and that lookup is strict — src/state.rs lines 3314-3322 keys flow_versions by (flowId, versionNumber) and returns None when no record sits under the pair (src/state.rs line 2604). So the 200 means the version record is still IN the store at the moment of the read. Hypothesis: nothing ever removes a numbered version from flow_versions, because the only operation that can — DeleteFlowVersion — is an unimplemented scaffold stub: services/.../src/ops/delete_flow_version.rs line 4 carries "//! SCAFFOLDED, NOT IMPLEMENTED" and its handle() body (lines 156-174) ends in Err(crate::scaffolded::Scaffolded::not_implemented("DeleteFlowVersion")), binding its input without touching state. A version therefore survives every delete the fixture performs, and the following read keeps reporting it.

Why: a client cannot observe a version delete at all. Version-retention and cleanup logic (prune old versions, then confirm the version is gone; re-cut a version number; assert a pinned version is no longer deployable) reads 200 for a version AWS reports 404 for, so it concludes the resource still exists and either loops forever waiting for the 404 or treats the stale snapshot as live. Because the response is a success, there is no error code or message to branch on either — the SDK's ResourceNotFoundException branch for GetFlowVersion never runs, and callers that distinguish "the flow is gone" (resource type Flow) from "this version is gone" (resource type FlowVersion) lose that signal entirely.

Fix: the flow-version state slice and the delete that must mutate it — implement DeleteFlowVersion in services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/delete_flow_version.rs (and drop its SCAFFOLDED marker) so an accepted delete REMOVES the (flowId, versionNumber) entry from the flow_versions map in src/state.rs, leaving GetFlowVersion's existing not-found branch to answer the modeled 404 with the FlowVersion wording. Re-verify GetFlowVersion's reader after that: it must keep refusing a resolvable flow's missing version as FlowVersion rather than Flow.

Traces: bedrock-agent-flow-prepare-and-version#task#9.

Not covered by any open issue — no filed issue carries this fixture step's replay key. 20261107T230358Z-bedrock-agent-4-net-new-causes-createflowalias-has.md describes DeleteFlowVersion's 501 from a BEHAVIORAL check (its missing alias-in-use ConflictException guard) and carries only behavioral check ids, not this wire step, so this divergence is not gated by it; a fixer closing that issue should re-replay this step too. 20261108T082251Z-bedrock-agent-flow-deployment-plane-a-prepared-flow.md covers the preparation-span 409 on CreateFlowVersion, the missing TSTALIASID alias and CreateFlowAlias's not-found resource type in other fixtures, not this read-after-delete.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 195 steps, 141 passed, score 72.3%).

Cause 1 — bedrock-agent-flow-prepare-and-version#task#9, GetFlowVersion (cluster affected_count=1):
  status: expected 404, got 200
  $.message: missing (expected "Couldn't perform GetFlowVersion operation. Failed to retrieve resource type FlowVersion because it doesn't exist. Check the resource ID and retry your request.", got None)
  $headers.x-amzn-errortype: missing (expected the ResourceNotFoundException error-type header, got None)
  Expected (AWS): HTTP 404 ResourceNotFoundException. Message template fixed text: "Couldn't perform <operation> operation. Failed to retrieve resource type <resourceType> because it doesn't exist. Check the resource ID and retry your request."; variable parts: <operation> = GetFlowVersion (from the request), <resourceType> = the type that failed to resolve = FlowVersion (from the emulator's state — the flow resolves, the version does not). No identifier is interpolated.
  Actual (emulator): HTTP 200 with a GetFlowVersionResponse for the version, and therefore no error code, no message and no error-type header.

Smithy (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/smithy-model/com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda.json):
  GetFlowVersion http {GET /flows/{flowIdentifier}/versions/{flowVersion}/, code 200}; input flowIdentifier @required @httpLabel, flowVersion NumericalVersion @pattern ^[0-9]{1,5}$ @required @httpLabel, includedData @httpQuery; errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400); readonly.
  DeleteFlowVersion http {DELETE /flows/{flowIdentifier}/versions/{flowVersion}/, code 200}; output {id, version} required; errors include ConflictException(409) and ResourceNotFoundException(404).

Source: services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/get_flow_version.rs lines 212-227 (flow not-found then flow_version_not_found_message branch — correct, but unreachable here); src/state.rs line 2604 (flow_versions keyed by (flowId, versionNumber)) and lines 3314-3322 (flow_version_of is a strict lookup on that pair, so a 200 proves the record is still stored); src/ops/delete_flow_version.rs line 4 ("//! SCAFFOLDED, NOT IMPLEMENTED") and lines 156-174 (handle() binds its input and returns Scaffolded::not_implemented("DeleteFlowVersion") without removing anything from state).

## Steps to Reproduce
1. Boot the emulator from this checkout and point a bedrock-agent client at it.
2. CreateFlow with a valid minimal definition and a flow execution role ARN; note <flowId>. Call PrepareFlow and poll GetFlow until status is Prepared. Call CreateFlowVersion(flowIdentifier=<flowId>) to cut version '1'.
3. GetFlowVersion(flowIdentifier=<flowId>, flowVersion='1') -> HTTP 200 (expected, the version exists).
4. Delete that numbered version: DeleteFlowVersion(flowIdentifier=<flowId>, flowVersion='1'). Observed: HTTP 501 NotImplemented and the version is left in the store.
5. GetFlowVersion(flowIdentifier=<flowId>, flowVersion='1') again, with the flow itself still present.
   Observed: HTTP 200 with the version's full representation — no message, no x-amzn-errortype.
   Expected: HTTP 404 ResourceNotFoundException with "Couldn't perform GetFlowVersion operation. Failed to retrieve resource type FlowVersion because it doesn't exist. Check the resource ID and retry your request." (resource type FlowVersion, not Flow, because the flow still resolves).
6. Replay fixture bedrock-agent-flow-prepare-and-version, step task#9, to reproduce the divergence from the recording.
