Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-gateway-target-workflow
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-gateway-target-workflow
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteGatewayTarget|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|DeleteGatewayTarget|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|DeleteGatewayTarget|4xx->4xx|status
Issue-Commit: b9efb47143cf6ff4809e1d58bc20d5c480ad9c3b

# DeleteGatewayTarget skips the gateway-status precondition and answers 404 ResourceNotFoundException instead of 400 ValidationException

> _Note: this issue was identified against the code base at commit `b9efb47143cf6ff4809e1d58bc20d5c480ad9c3b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteGatewayTarget

## Description
Cause 1 of 1: DeleteGatewayTarget does not gate the delete on the parent gateway's lifecycle state, and the target it is asked to delete is not resolvable, so the call is refused as a missing resource instead of as an invalid operation for the gateway's current status.

AWS: Smithy: DeleteGatewayTarget (DELETE /gateways/{gatewayIdentifier}/targets/{targetId}/, success code 202) models both ValidationException (httpError 400, "The input fails to satisfy the constraints specified by the service") and ResourceNotFoundException (httpError 404). GatewayStatus enum = CREATING | UPDATING | UPDATE_UNSUCCESSFUL | DELETING | READY | FAILED. While the parent gateway has not reached READY, AWS refuses a target mutation on it as a precondition failure rather than as a lookup failure: the recorded response is HTTP 400 with x-amzn-errortype: ValidationException and message 'Cannot perform operation DeleteGatewayTarget when gateway is in CREATING status'. In that message the fixed text is 'Cannot perform operation ', ' when gateway is in ' and ' status'; the variable parts are the operation name taken from the request (DeleteGatewayTarget) and the parent gateway's current GatewayStatus value taken from service state (CREATING here, but any non-terminal status the gateway actually holds). The targetId/gatewayIdentifier of the recording are per-run values and are not part of the requirement.

Emulator: answers HTTP 404 with x-amzn-errortype: ResourceNotFoundException and message "Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier." — i.e. it goes straight to a target lookup, never consults the parent gateway's status, and additionally reports the target created earlier in the same fixture as non-existent.

Why: a client cannot distinguish 'the gateway is still coming up, retry after it is READY' from 'this target id is wrong'. ValidationException/400 is a precondition the caller resolves by waiting or by polling GetGateway; ResourceNotFoundException/404 tells the caller the resource is gone, so SDK and user retry/cleanup logic abandons a target that in fact still exists, and teardown paths that treat 404 as 'already deleted' silently leak the target. The wrong x-amzn-errortype also routes to the wrong modeled exception type in every generated SDK.

Fix: the DeleteGatewayTarget handler's validation — resolve the parent gateway first and, when its GatewayStatus is not a state that permits target mutation, return ValidationException (400) with the 'Cannot perform operation <Operation> when gateway is in <Status> status' message before any target lookup; and ensure the gateway-target state keyed by (gatewayIdentifier, targetId) is the same store CreateGatewayTarget writes, so an existing target is not reported missing.

Traces: bedrock-agentcore-control-gateway-target-workflow#task#4 (expected 400 ValidationException, got 404 ResourceNotFoundException).

Note: no open issue covers this operation — the nearest open bedrock-agentcore-control issues (20261118T191327Z-bedrock-agentcore-control-5-net-new-causes-in.md, 20261121T032543Z-bedrock-agentcore-control-deletegatewayratelimit-answers-404-for-a-rate-limit-the.md, 20261102T055559Z-...creategateway-and-getgateway-gateway-representation-diverges-gatewayurl-carries.md) are about rate limits, online-evaluation configs and the gateway representation, not gateway targets.

## Evidence
Fixture bedrock-agentcore-control-gateway-target-workflow, step task#4, operation DeleteGatewayTarget.

Expected (recorded AWS response):
  status: 400
  headers: x-amzn-errortype: ValidationException
  body: $.message = 'Cannot perform operation DeleteGatewayTarget when gateway is in CREATING status'

Actual (emulator):
  status: 404
  headers: x-amzn-errortype: ResourceNotFoundException
  body: $.message = "Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier."

Replay detail verbatim: "status: expected 400, got 404 | $.message: value_mismatch (expected 'Cannot perform operation DeleteGatewayTarget when gateway is in CREATING status', got \"Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier.\") | $headers.x-amzn-errortype: value_mismatch (expected 'ValidationException', got 'ResourceNotFoundException')"

Model grounding (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model): DeleteGatewayTarget binds DELETE /gateways/{gatewayIdentifier}/targets/{targetId}/ with success 202 and errors AccessDeniedException(403), ConflictException(409), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). GatewayStatus includes CREATING and READY.

## Steps to Reproduce
1. Replay the wire corpus simparity-wire-aws-v1 against the emulator.
2. Fixture bedrock-agentcore-control-gateway-target-workflow: CreateGateway (setup#1, gateway is returned in CREATING status), CreateGatewayTarget against that gateway, then at task#4 call DeleteGatewayTarget with that gatewayIdentifier and the created targetId while the gateway has not yet reached READY.
3. Observe the response: the emulator answers 404 ResourceNotFoundException ("Failed to retrieve target because it doesn't exist..."); AWS answers 400 ValidationException with x-amzn-errortype: ValidationException and 'Cannot perform operation DeleteGatewayTarget when gateway is in CREATING status'.

## Resolution

Cause 1 is FIXED, and the precondition the issue asks for was not what was missing. `DeleteGatewayTarget`
already resolves the parent gateway and refuses a non-terminal status with the recorded sentence BEFORE it
looks the target up, and the targets it reads are the ones `CreateGatewayTarget` writes (one map on the
gateway record). What made that refusal unreachable was the gateway family's transient window: it was armed
for a span of the order of one round trip to this endpoint, so the gateway reported `CREATING` to the
caller's second call and `READY` to its fourth, and which refusal a workflow heard depended on how fast its
calls were dispatched. The window is now sized a long way above a round trip, the span the sibling resource
families of this service already use, so every state precondition keyed on a gateway's status is decided the
same way on a fast host and on a slow one.

Applied where the rule holds: `UpdateGatewayTarget` resolved the target before consulting the gateway's
status, so it answered the lookup failure where the family answers the precondition; the two are now in the
family's order. `CreateGatewayTarget`, `DeleteGatewayTarget` and `SynchronizeGatewayTargets` already had it.

Conflicting unit, kept on the record: the behavioural unit of
`20261120T051930Z-bedrock-agentcore-control-a-gateway-s-creating-ready.md` issues a SERVED `DeleteGateway`
five calls after the create, while this fixture's refusals are recorded two and four calls after it.
Replayed in process both sequences complete in the same handful of milliseconds, so no wall-clock span
separates them as the recordings' own latencies did. The window follows the recorded refusals, because a
span of the order of one call makes the emulator's answer a race rather than a rule; a caller that waits for
the status it needs by polling the gateway reaches it.
