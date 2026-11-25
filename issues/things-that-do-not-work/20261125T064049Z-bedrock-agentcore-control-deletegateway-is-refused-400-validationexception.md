Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteGateway|ValidationException
Issue-Commit: e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14

# bedrock-agentcore-control: DeleteGateway is refused 400 ValidationException "...in Creating state" for a gateway the caller created earlier in the same sequence

> _Note: this issue was identified against the code base at commit `e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteGateway

## Description
Cause 1 of 1: DeleteGateway refuses a gateway that has already finished being created, because the emulator's gateway status clock still reports CREATING when the delete arrives (failing check: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied).

AWS: recorded behavior only — the Smithy model documents no dwell for a gateway's CREATING status (CreateGateway and DeleteGateway both answer HTTP 202; DeleteGatewayResponse.status is the GatewayStatus enum CREATING | DELETING | FAILED | READY | UPDATE_UNSUCCESSFUL | UPDATING, and DeleteGateway's modeled errors are AccessDenied 403 / Conflict 409 / InternalServer 500 / ResourceNotFound 404 / Throttling 429 / Validation 400). The recording shows what AWS does: a client calls CreateGateway (authorizerType NONE), then does a handful of unrelated calls (iam:CreateRole, iam:PutRolePolicy, sts:AssumeRole, a denied DeleteGateway from the restricted principal, GetGateway), and then calls DeleteGateway as the gateway's owner WITHOUT ever polling for READY. AWS accepts that delete: it answers 202 with gatewayId equal to the created gateway and a DELETING-class status, and GetGateway then settles on ResourceNotFoundException with the message "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier." In other words, by the time a client's next few API calls have completed, a newly created gateway is out of CREATING and a delete is accepted.

Emulator: the same owner DeleteGateway is refused HTTP 400 ValidationException with the message "DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state." Fixed text of that message class: "<Operation> operation can't be performed on gateway when it is in <Status-in-prose> state. Retry the request when the gateway is in a valid state."; its variable parts are the operation name and the gateway's current status, taken from the request and the record. The refusal itself is correct for a gateway that genuinely is CREATING — the defect is that the gateway is still reported CREATING at this point in the sequence at all. The gateway's transient CREATING status is held until a wall-clock instant stored at create time, and the surrounding client calls against a local emulator complete faster than that window, so a create-then-delete sequence never sees the gateway settle. Hypothesis: the CREATING dwell is expressed as a fixed wall-clock span rather than as "settled by the time the caller's next request arrives", so against an emulator with sub-millisecond latencies the window outlives the whole client sequence AWS completes it in.

Why: a client that provisions a gateway and tears it down (or deletes and recreates it) in one program — the ordinary teardown path, and exactly what test fixtures and CloudFormation-style rollbacks do — gets a hard 400 ValidationException from the emulator where AWS accepts the delete with 202. The caller has no signal to retry on (it never asked for READY, and AWS never required it to), so the resource leaks and the client errors out. The same window makes every other mutation of a just-created gateway (UpdateGateway, target/rule creation) racy against the emulator but not against AWS.

Fix: the gateway status clock in the service state (GatewayRecord::status_at / the CREATING→READY transition armed by CreateGateway) and the status gate in DeleteGateway's handler that turns a non-READY status into ValidationException — a gateway must have settled to READY by the time any subsequent request reads it, rather than after a fixed wall-clock span that a local client outruns.

Traces: bedrock-agentcore-control.delete-gateway-without-iam-permission-denied. Earlier assertions of that check pass (the restricted principal's DeleteGateway is correctly refused AccessDeniedException with the exact IAM message, and GetGateway shows the gateway untouched), so the only divergence is the owner's delete being refused. Relevant source: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_gateway.rs lines 195-202 (the status gate), src/ops/create_gateway.rs lines 325-329 (transient CREATING + settles_at), src/state.rs lines 994-1000 (GatewayRecord::status_at) and src/state.rs line 4144 (GATEWAY_SETTLE_DELAY).

## Evidence
Expected (AWS, recorded): the owner's DeleteGateway immediately after the create/assume-role/denied-delete/GetGateway sequence is ACCEPTED — HTTP 202, body carrying gatewayId == the created gateway id and a DELETING-class status; a following GetGateway poll ends in 404 ResourceNotFoundException "Failed to retrieve gateway because it doesn't exist. Retry the request with a different resource identifier."

Actual (emulator): botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the DeleteGateway operation: DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state.  (HTTP 400; raised out of run() at the owner's `client.delete_gateway(gatewayIdentifier=gateway_id)` call, so the check never reaches the post-delete ResourceNotFoundException assertions.)

## Steps to Reproduce
1. CreateGateway(name=<unique>, roleArn=arn:aws:iam::<account>:role/<any-role>, authorizerType="NONE") and keep gatewayId.
2. Create an IAM role with no bedrock-agentcore permissions, assume it, and call DeleteGateway(gatewayIdentifier=<gatewayId>) with those credentials — observe the correct 403 AccessDeniedException.
3. As the owning caller, call GetGateway(gatewayIdentifier=<gatewayId>) — it answers, status not DELETING.
4. As the owning caller, call DeleteGateway(gatewayIdentifier=<gatewayId>) WITHOUT polling for READY first.
   AWS: 202 with gatewayId and a DELETING-class status; subsequent GetGateway eventually 404 ResourceNotFoundException.
   Emulator: 400 ValidationException "DeleteGateway operation can't be performed on gateway when it is in Creating state. Retry the request when the gateway is in a valid state."
