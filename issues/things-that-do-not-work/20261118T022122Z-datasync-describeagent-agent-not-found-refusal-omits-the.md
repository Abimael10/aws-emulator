Issue-Source: trace-sweep-divergence
Issue-Trace: datasync-agent-dependent-location-validation
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: datasync-agent-dependent-location-validation
Issue-Shape-Id: com.amazonaws.datasync#FmrsService
Issue-Key: datasync|DescribeAgent|4xx->4xx|$.datasyncErrorCode
Issue-Key: datasync|DescribeAgent|4xx->4xx|$.errorCode
Issue-Commit: 0d9998861fdf39504f91db811334b4220d543cae

# DataSync DescribeAgent agent-not-found refusal omits the errorCode/datasyncErrorCode discriminator AgentNotFound

> _Note: this issue was identified against the code base at commit `0d9998861fdf39504f91db811334b4220d543cae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAgent

## Description
Cause 1 of 1: DescribeAgent refuses an AgentArn that names no agent of the account with the right HTTP status, the right error shape and the right message, but serializes the refusal WITHOUT the two error-code discriminator members the shape carries, so a client cannot tell this refusal apart from any other InvalidRequestException.

AWS: Smithy (model services/service-com-amazonaws-datasync-fmrsservice/smithy-model/com-amazonaws-datasync-fmrsservice.json, service com.amazonaws.datasync#FmrsService) models DescribeAgent with input AgentArn (required, @length 0..128, pattern ^arn:(aws|aws-cn|aws-us-gov|aws-eusc|aws-iso|aws-iso-b):datasync:[a-z\-0-9]+:[0-9]{12}:agent/agent-[0-9a-z]{17}$) and exactly two errors: InternalException (HTTP 500, server) and InvalidRequestException (HTTP 400, client, "thrown when the client submits a malformed request"). Shape InvalidRequestException has three members: message, errorCode and datasyncErrorCode. Recorded behavior: a syntactically valid AgentArn that resolves to no agent of the account is refused with HTTP 400, __type InvalidRequestException, message "<agent-id> does not exist or has been deleted." AND both discriminators populated with the same token, errorCode = "AgentNotFound" and datasyncErrorCode = "AgentNotFound". The requirement is the pairing: for the unknown-agent class of InvalidRequestException both errorCode and datasyncErrorCode are the fixed literal "AgentNotFound" (the token is the error class, not a value from the request); the message's fixed text is " does not exist or has been deleted." with the agent resource id (the agent-<17 chars> token parsed off the request's AgentArn) as its only variable part. The region, account and agent id of any one recording are per-run values and are not part of the requirement.

Emulator: DescribeAgent answers HTTP 400 with InvalidRequestException and the correct message, but emits no errorCode and no datasyncErrorCode at all — the replay reports "$.datasyncErrorCode: missing (expected 'AgentNotFound', got None)" and "$.errorCode: missing (expected 'AgentNotFound', got None)" with expected_status 400 == actual_status 400. The refusal is built in one shared constructor that hard-codes both discriminators to None: services/service-com-amazonaws-datasync-fmrsservice/src/state/agents.rs:117-125 agent_not_found() sets error_code: None, datasync_error_code: None, and its doc comment asserts the refusal "carries no class discriminator", which is what the recording contradicts. Because DescribeAgent, UpdateAgent and DeleteAgent all resolve through state::agents (agent(), agent_mut(), remove_agent()), every agent-surface refusal is missing the token, not just this read.

Why: errorCode/datasyncErrorCode is how a DataSync client classifies a 400 — InvalidRequestException is the single modeled client error for the whole service, so the discriminator is the only machine-readable statement of WHICH validation failed. A client that branches on AgentNotFound (re-register or re-activate the agent, surface the agent id to the operator, stop retrying) sees a null discriminator and falls through to its generic "malformed request" path, which is the wrong remediation for a deleted agent; code that reads the field unconditionally gets a null where AWS always supplies a string.

Fix: the shared refusal constructor in the datasync service crate's agent state module — agent_not_found() in state/agents.rs must populate both errorCode and datasyncErrorCode with "AgentNotFound" alongside the message it already builds, so every operation resolving an agent through agent()/agent_mut()/remove_agent() reports the class. Note this is the same defect class as the open issue 20261113T084741Z-datasync-createlocations3-misdiagnoses-an-unassumable-bucket-access-role.md (missing S3NoSuchBucket discriminator on CreateLocationS3) but a different constructor and a different operation, and the open issue 20261117T214328Z-datasync-agent-arn-existence-refusal-is-unreachable-createlocationnfs.md covers only the 501 of CreateLocationNfs/Hdfs/Smb and explicitly treats DescribeAgent's refusal as correct; neither carries this step's replay key.

Traces: fixture datasync-agent-dependent-location-validation step task#1 (DescribeAgent on a valid-but-unregistered AgentArn).

## Evidence
Fixture datasync-agent-dependent-location-validation, step task#1, operation DescribeAgent.

Expected (recorded AWS response), HTTP 400:
  __type              = InvalidRequestException
  errorCode           = "AgentNotFound"
  datasyncErrorCode   = "AgentNotFound"
  message             = "<agent-id> does not exist or has been deleted."   (fixed text " does not exist or has been deleted."; variable part = the agent-<17 chars> id taken from the request's AgentArn)

Actual (emulator), HTTP 400:
  __type              = InvalidRequestException
  errorCode           = <absent>
  datasyncErrorCode   = <absent>
  message             = matches (not reported as diverging)

Replay detail, verbatim:
  $.datasyncErrorCode: missing (expected 'AgentNotFound', got None)
  $.errorCode: missing (expected 'AgentNotFound', got None)
  status: expected 400, got 400 (status agrees; only the discriminators diverge)

Source of the omission: services/service-com-amazonaws-datasync-fmrsservice/src/state/agents.rs:117 agent_not_found() -> InvalidRequestException { message: Some(...), error_code: None, datasync_error_code: None }. Smithy: InvalidRequestException members = message, errorCode, datasyncErrorCode; DescribeAgent errors = InternalException (500), InvalidRequestException (400).

## Steps to Reproduce
1. Boot the datasync emulator from this checkout.
2. Build a syntactically valid but unregistered agent ARN: arn:aws:datasync:<region>:<account>:agent/agent-<17 lowercase alphanumerics> (matching the modeled AgentArn pattern).
3. Call DescribeAgent with that ARN.
4. Observe HTTP 400 with __type InvalidRequestException and the correct "<agent-id> does not exist or has been deleted." message, but with errorCode and datasyncErrorCode absent instead of both set to "AgentNotFound".
5. Re-replay fixture datasync-agent-dependent-location-validation step task#1 to confirm both discriminators are present and equal to "AgentNotFound"; also re-check UpdateAgent/DeleteAgent on an unregistered ARN, which share the same agent_not_found() constructor.
