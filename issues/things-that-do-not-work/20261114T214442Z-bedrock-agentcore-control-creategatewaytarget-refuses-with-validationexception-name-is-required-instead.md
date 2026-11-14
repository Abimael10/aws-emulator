Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-gateway-error-paths
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-gateway-error-paths
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|CreateGatewayTarget|4xx->4xx|$.message
Issue-Related: 20261114T073009Z-bedrock-agentcore-control-2-net-new-causes-creategatewaytarget.md
Issue-Commit: cf4f60c5fc13ba8044ce457fda63f711f871a11a

# CreateGatewayTarget refuses with ValidationException 'name: is required' instead of 404 ResourceNotFoundException for an unknown gateway

> _Note: this issue was identified against the code base at commit `cf4f60c5fc13ba8044ce457fda63f711f871a11a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateGatewayTarget

## Description
Cause 1 of 1: CreateGatewayTarget never resolves the gateway named by its path label, and refuses the request with a fabricated required-member validation error instead of reporting that the gateway does not exist.

AWS: Smithy CreateGatewayTarget binds POST /gateways/{gatewayIdentifier}/targets/ with success code 202. Its input declares gatewayIdentifier as @httpLabel @required (GatewayIdentifier, pattern ^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$) and targetConfiguration as @required; `name` (TargetName, pattern ^([0-9a-zA-Z][-]?){1,100}$, @sensitive) is NOT @required, so a request that omits it is legal. The operation models ResourceNotFoundException (httpError 404) and ValidationException (httpError 400). AWS resolves the gateway from the path label BEFORE validating the request body, and when no gateway with that identifier exists it answers HTTP 404 with x-amzn-errortype: ResourceNotFoundException and the message "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier." That message is entirely fixed text for this error class — it has no variable part, and AWS does not interpolate the requested gateway identifier into it (recorded behavior confirms the fixed wording; the model supplies the error shape and status).

Emulator: answers HTTP 400 with x-amzn-errortype: ValidationException and the message `name: is required`. The gateway identifier carried in the path is never looked up, and the refusal is attributed to an input member the model does not require.

Why: a client targeting a gateway that was deleted or never created cannot distinguish "the gateway is gone" from "my request is malformed". ValidationException/400 is classified as a non-retryable client programming error, so the client never takes the recreate-or-resolve-the-gateway path that ResourceNotFoundException/404 signals, and generic retry and error-mapping layers branch on the status as well as the error code. The fabricated required-check compounds this: a legitimate CreateGatewayTarget against an EXISTING gateway that relies on `name` being optional is rejected too, making that shape of request unreachable.

Fix: the CreateGatewayTarget handler and its input validation — resolve the path's gatewayIdentifier against gateway state and raise ResourceNotFoundException (404, x-amzn-errortype: ResourceNotFoundException, AWS's fixed message) before any body validation runs, and drop the `name` required-check, which the model does not state. Related open issues that do NOT cover this error path: 20261114T073009Z-bedrock-agentcore-control-2-net-new-causes-creategatewaytarget.md describes this same gateway-existence gap but its replay keys cover only the status and x-amzn-errortype deltas, so the replay reports this cluster (which also diverges on $.message) as unowned and it is still reproducing after that issue was filed; 20261108T153618Z-bedrock-agentcore-control-creategatewaytarget-settles-every-mcp-mcpserver-target-ready-with.md concerns the post-create READY settling on the success path only.

Traces: bedrock-agentcore-control-gateway-error-paths#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 296 steps, 182 passed, 110 mismatched, 4 blocked, score 61.5%, 36 divergence clusters. This is the only cluster with no blocking stub and no owning open issue.

Cause 1 — CreateGatewayTarget, fixture bedrock-agentcore-control-gateway-error-paths step task#1 (cluster affected_count 1):
  status: expected 404, got 400
  $headers.x-amzn-errortype: expected 'ResourceNotFoundException', got 'ValidationException'
  $.message: expected "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier." (entirely fixed text for this error class; no request- or state-derived variable part), got 'name: is required'

Model citations (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json): CreateGatewayTarget http {method: POST, uri: /gateways/{gatewayIdentifier}/targets/, code: 202}; input.gatewayIdentifier @httpLabel @required pattern ^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$; input.targetConfiguration @required; input.name present but NOT required; errors include ResourceNotFoundException (httpError 404) and ValidationException (httpError 400).

## Steps to Reproduce
1. POST /gateways/<a gatewayIdentifier matching ^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$ that was never created>/targets/ with a valid targetConfiguration and no `name` member.
2. Observe the emulator answer HTTP 400, x-amzn-errortype: ValidationException, message 'name: is required'.
3. AWS answers HTTP 404, x-amzn-errortype: ResourceNotFoundException, message "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier."
4. Re-replay to confirm the fix: fixture bedrock-agentcore-control-gateway-error-paths step task#1.
