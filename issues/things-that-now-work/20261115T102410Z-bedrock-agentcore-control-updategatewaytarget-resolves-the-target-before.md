Issue-Source: trace-sweep-divergence
Issue-Trace: acc-gateway-update-and-target-errors
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acc-gateway-update-and-target-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|UpdateGatewayTarget|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|UpdateGatewayTarget|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|UpdateGatewayTarget|4xx->4xx|status
Issue-Commit: 39ac78f4c744f316ef870f9b4f7492229bfc440d

# bedrock-agentcore-control: UpdateGatewayTarget resolves the target before validating the body, answering ResourceNotFoundException where AWS answers ValidationException for a missing credentialProviderConfigurations

> _Note: this issue was identified against the code base at commit `39ac78f4c744f316ef870f9b4f7492229bfc440d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateGatewayTarget

## Description
One net-new cause in the bedrock-agentcore-control emulator, found by replaying the wire fixture corpus. It is not covered by an open issue: the nearest neighbour is 20261114T041101Z-bedrock-agentcore-control-3-net-new-causes-vpc.md, whose Cause 2/Cause 3 reason about the SAME operation from the behavioral suite but assert the opposite contract (that AWS accepts an UpdateGatewayTarget which omits credentialProviderConfigurations, and that target resolution must run BEFORE input validation). This recorded wire step is direct evidence against that reading and must be fixed on its own terms; a fixer closing that issue's Cause 2/3 without this one will re-introduce this divergence.

Cause 1 of 1: UpdateGatewayTarget reports a target-resolution failure where AWS reports a missing-input failure, because the emulator resolves (gatewayIdentifier, targetId) against state before it validates the request body.

AWS: Smithy UpdateGatewayTarget (PUT /gateways/{gatewayIdentifier}/targets/{targetId}/, http code 202) models both ValidationException (httpError 400) and ResourceNotFoundException (httpError 404). The input shape UpdateGatewayTargetRequest marks gatewayIdentifier, targetId and targetConfiguration @required and leaves credentialProviderConfigurations (list, @length 1..1) unmarked, but the output shape UpdateGatewayTargetResponse marks credentialProviderConfigurations @required. Recorded behavior shows what the service actually enforces: for an UpdateGatewayTarget request that omits credentialProviderConfigurations, AWS refuses the call with HTTP 400 and the message "Credential provider configurations is not defined" — and it does so for THIS request, in which the addressed target cannot be resolved, i.e. the body-completeness check has precedence over resolving the target. The message's text is entirely fixed: it is a whole sentence naming the missing input member, with no part taken from the request or from state. The error class for that message is ValidationException (the fixture does not assert x-amzn-errortype, so only the 400 status and the message text are requirements).

Emulator: the step is answered HTTP 404 with x-amzn-errortype: ResourceNotFoundException and the message "Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier." (that sentence is itself the correct fixed text AWS words for a missing target — it is emitted for the wrong request). The emulator never reaches the body check, so the absence of credentialProviderConfigurations is never reported. Hypothesis: the handler looks the target up as its first step and returns early, so input validation is dead code for any unresolvable target id.

Why: a client cannot tell a malformed request from a vanished resource. On AWS this request is a caller-input bug to be fixed and never retried (400 ValidationException naming the missing member); against the emulator the same request looks like a resource lifecycle problem (404 ResourceNotFoundException), so retry/recreate logic keyed on ResourceNotFoundException re-creates or re-polls a target to work around a request the service would never accept, and SDK-level validation tests that assert the 400/"Credential provider configurations is not defined" refusal pass against AWS and fail against the emulator (and vice versa).

Fix: UpdateGatewayTarget's handler in the bedrock-agentcore-control service crate must run request-body validation before resolving the addressed target — when credentialProviderConfigurations is absent, refuse with ValidationException (HTTP 400) and AWS's fixed sentence "Credential provider configurations is not defined"; only once the body passes may it resolve (gatewayIdentifier, targetId) and raise ResourceNotFoundException (404) with the missing-target sentence. The ordering, not just the message, is the requirement.

Traces: acc-gateway-update-and-target-errors#task#7.

## Evidence
Cause 1 — fixture acc-gateway-update-and-target-errors, step task#7, operation UpdateGatewayTarget (service bedrock-agentcore-control):
  Expected (recorded AWS): HTTP 400, body $.message == "Credential provider configurations is not defined" (fixed text, no variable parts), error class ValidationException ($headers.x-amzn-errortype recorded as <shaped-not-asserted>, so the status and message are the asserted requirements).
  Actual (emulator): HTTP 404, $headers.x-amzn-errortype == "ResourceNotFoundException", $.message == "Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier."
  Replay detail verbatim: "status: expected 400, got 404 | $.message: value_mismatch (expected 'Credential provider configurations is not defined', got \"Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier.\") | $headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'ResourceNotFoundException')"
  Model citation: Smithy UpdateGatewayTarget errors include ValidationException (400) and ResourceNotFoundException (404); UpdateGatewayTargetRequest requires gatewayIdentifier, targetId, targetConfiguration, while UpdateGatewayTargetResponse requires credentialProviderConfigurations (@length 1..1).
  Nothing in this divergence depends on a per-run value: no host, port, generated id or timestamp appears in either the expected or the actual message.

## Steps to Reproduce
1. Boot the emulator and replay the wire corpus (trace set simparity-wire-aws-v1), or drive the steps by hand against the bedrock-agentcore-control endpoint:
2. Create a gateway (CreateGateway) and let it settle, as fixture acc-gateway-update-and-target-errors does in its setup phase.
3. Issue UpdateGatewayTarget for that gateway with a targetId that does not exist, supplying only the required input members (gatewayIdentifier, targetId, targetConfiguration) and OMITTING credentialProviderConfigurations:
   PUT /gateways/<gatewayIdentifier>/targets/<nonexistent-targetId>/  body: {"targetConfiguration": {"mcp": {...}}}
4. Observe HTTP 404 with x-amzn-errortype: ResourceNotFoundException and "Failed to retrieve target because it doesn't exist. Retry the request with a different resource identifier."; AWS answers HTTP 400 ValidationException "Credential provider configurations is not defined" because the body check precedes target resolution.
5. Re-replay fixture acc-gateway-update-and-target-errors step task#7 to confirm a fix.
