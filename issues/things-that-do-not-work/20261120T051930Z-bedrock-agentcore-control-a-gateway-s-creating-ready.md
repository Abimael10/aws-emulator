Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteGateway|ValidationException
Issue-Commit: e79672e854f5973dd19ff8b089aaca728dc35125

# bedrock-agentcore-control: a gateway's CREATING→READY transition is pinned to a fixed 500 ms wall-clock span, so DeleteGateway on a gateway the caller has already read back is refused 400 ValidationException "...in Creating state" instead of being accepted

> _Note: this issue was identified against the code base at commit `e79672e854f5973dd19ff8b089aaca728dc35125`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteGateway

## Description
Cause 1 of 1: DeleteGateway refuses a gateway that AWS has long since made READY, because the emulator makes the gateway's CREATING→READY transition due only after a fixed 500 ms of wall-clock time measured from CreateGateway, and every gateway mutation is hard-gated on READY.

AWS: Smithy: CreateGateway is async (HTTP 202, `status` GatewayStatus one of CREATING | DELETING | FAILED | READY | UPDATE_UNSUCCESSFUL | UPDATING); DeleteGateway is HTTP 202 `{gatewayId, status, statusReasons?}` with errors AccessDeniedException 403, ConflictException 409, ResourceNotFoundException 404, ValidationException 400. The model says nothing about how long CREATING lasts — recorded behavior only: in the recording the caller creates a gateway, then does an IAM/STS round trip (CreateRole + PutRolePolicy + AssumeRole), then issues DeleteGateway as the owner and it is ACCEPTED (202, gatewayId echoed), after which GetGateway answers 404 ResourceNotFoundException with "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier." AWS does refuse a delete while the gateway is still CREATING (that part is faithful) — but on AWS a gateway is READY well before a caller can complete an IAM role create + assume, so the recorded delete is never refused.

Emulator: the gateway is still reported CREATING when the delete arrives, and DeleteGateway answers 400 ValidationException with message "DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state." (fixed text: "<Operation> operation can't be performed on gateway when it is in <Status> state. Retry the request when the gateway is in a valid state."; variable parts are the asking operation's name, here DeleteGateway, and the gateway's current status spelled in prose, here Creating — the wording itself is not the divergence). Hypothesis, from the handler and the store: CreateGateway writes transient_status=CREATING, settled_status=READY, settles_at=now+GATEWAY_SETTLE_DELAY, and GATEWAY_SETTLE_DELAY is 500 ms; GatewayRecord::status_at(now) therefore reports CREATING for the first half second after the create, and DeleteGateway returns the status-conflict ValidationException for any status != READY. Because this emulator answers IAM CreateRole/PutRolePolicy and STS AssumeRole instantly (no eventual-consistency wait), the whole create → IAM round trip → delete sequence completes inside that 500 ms window, so the delete lands while the gateway is artificially still CREATING. The gateway is never deleted and the trailing 404 contract is never exercised.

Why: a client that creates a gateway and then acts on it — the ordinary create-then-configure-or-tear-down script, and every test that does a create/delete round trip — is refused 400 ValidationException by this emulator for work AWS accepts. The refusal is not reachable by any remedy the client controls other than sleeping for a duration the emulator invented, and nothing in the gateway representation tells the client the wait is a fixed local timer rather than real provisioning. A teardown that tolerates the conflict (as the recording's teardown does) then spins to its timeout, and the gateway leaks.

Fix: the gateway's settle schedule in the service state (GatewayRecord::status_at / GATEWAY_SETTLE_DELAY in services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state.rs, written by CreateGateway's handler) together with the READY gate in DeleteGateway's handler (services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_gateway.rs): the transient CREATING status must not outlive the caller's first round trip to the gateway, so a caller that has already read the gateway back is not refused its own delete. Keeping the CREATING status observable and keeping the delete refusal faithful are both required; what must change is that the window is a fixed wall-clock span independent of what the caller has observed.

Traces: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied (the AccessDeniedException probe and its exact IAM message pass; the positive-control owner delete that follows is the call that is refused).

## Evidence
Check: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied (verdict: error).

Expected (recorded AWS): the owner's `DeleteGateway(gatewayIdentifier=<gatewayId>)` after the denied-caller probe returns HTTP 202 with `gatewayId` equal to the created gateway's id, and a subsequent `GetGateway` raises 404 ResourceNotFoundException with message "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier."

Actual (emulator): botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the DeleteGateway operation: DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state.  — HTTP 400, error code ValidationException. The exception propagates out of the check (it is the unguarded positive-control delete, not the denied-caller probe, which is wrapped and did assert AccessDeniedException with its exact message).

Message anatomy of the emulator's refusal: fixed text "<op> operation can't be performed on gateway when it is in <status> state. Retry the request when the gateway is in a valid state."; variable parts "DeleteGateway" (the asking operation, from the request) and "Creating" (the gateway's status, from emulator state). The status value is what diverges — the gateway is reported CREATING where AWS reports READY by this point in the sequence.

Supporting code: services/.../src/ops/create_gateway.rs lines 333-337 set transient_status=STATUS_CREATING, settled_status=STATUS_READY, settles_at=now+GATEWAY_SETTLE_DELAY; services/.../src/state.rs line 3975 `pub const GATEWAY_SETTLE_DELAY: Duration = Duration::from_millis(500);`; services/.../src/state.rs lines 996-1000 GatewayRecord::status_at; services/.../src/ops/delete_gateway.rs lines 195-202 refuse any status != STATUS_READY with gateway_status_conflict.

## Steps to Reproduce
1. Boot the emulator and point a bedrock-agentcore-control client at it.
2. CreateGateway(name=<unique>, roleArn=arn:aws:iam::<account>:role/<any-role>, authorizerType=NONE) -> 202, status CREATING.
3. Immediately (within the same script, with no sleep): create an IAM role with a policy that grants only sts:GetCallerIdentity, AssumeRole into it, and call DeleteGateway with those restricted credentials -> 403 AccessDeniedException (correct).
4. GetGateway(gatewayIdentifier=<gatewayId>) with the owning credentials -> 200; observe `status` is still CREATING.
5. DeleteGateway(gatewayIdentifier=<gatewayId>) with the owning credentials -> observed 400 ValidationException "DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state."; AWS accepts the delete (202) and GetGateway then answers 404 ResourceNotFoundException "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier."
6. Replay: run the behavioral check bedrock-agentcore-control.delete-gateway-without-iam-permission-denied.
