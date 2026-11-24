Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: datasync.agent-not-found-error-is-consistent-across-location-types
Issue-Shape-Id: com.amazonaws.datasync#FmrsService
Issue-Key: datasync.agent-not-found-error-is-consistent-across-location-types|run#11|AssertionError|assert code == "InvalidRequestException", (
Issue-Commit: efa2324aebd17d792184f60c1e2f2af365d2b640

# DataSync agent-ARN existence refusal is unreachable: CreateLocationNfs/Hdfs/Smb answer 501 NotImplemented instead of InvalidRequestException "<agent-id> does not exist or has been deleted."

> _Note: this issue was identified against the code base at commit `efa2324aebd17d792184f60c1e2f2af365d2b640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLocationNfs

## Description
Cause 1 of 1: the agent-backed Create*Location family does not exist in the emulator, so the uniform unregistered-agent refusal AWS gives for every location family cannot be observed.

AWS: Smithy (com.amazonaws.datasync#FmrsService) models CreateLocationNfs (input Subdirectory/ServerHostname/OnPremConfig required, OnPremConfig.AgentArns), CreateLocationHdfs (NameNodes, AuthenticationType, AgentArns required, AgentArnList @length 1..8) and CreateLocationSmb (Subdirectory/ServerHostname/AgentArns required) each with exactly two errors: InternalException (HTTP 500, server) and InvalidRequestException (HTTP 400, client, "thrown when the client submits a malformed request"). Recorded behavior: a syntactically valid AgentArn (matching the modeled AgentArn pattern ^arn:...:datasync:<region>:<account>:agent/agent-[0-9a-z]{17}$) that names no registered agent is refused by every one of these operations with HTTP 400, error code InvalidRequestException, and the SAME message DescribeAgent gives for that ARN. The message's fixed text is " does not exist or has been deleted."; its one variable part is the agent id taken from the AgentArn in the request (the `agent-<17 chars>` resource part, not the whole ARN) — e.g. for AgentArns=[arn:aws:datasync:<region>:<account>:agent/agent-<id>] the message is "agent-<id> does not exist or has been deleted.". The refusal is about the named agent, not about the shape of the member: the same request family with no agent at all (CreateLocationObjectStorage without AgentArns) succeeds and the created location describes back, which this emulator already does correctly.

Emulator: DescribeAgent produces the correct InvalidRequestException and the correct "<agent-id> does not exist or has been deleted." message, but CreateLocationNfs fails the call with HTTP 501, error code 'NotImplemented', message "operation CreateLocationNfs is scaffolded but not implemented in this emulator". CreateLocationHdfs and CreateLocationSmb are in the same state (both handlers still carry the scaffold marker and return the same 501), so none of the three can ever agree with DescribeAgent. The replay asserts on the first of them and reports: "CreateLocationNfs with the unregistered agent failed with 'NotImplemented', expected 'InvalidRequestException'".

Why: a client that provisions NFS, HDFS or SMB transfer locations gets a 501/NotImplemented it can neither retry nor classify. Error-handling paths keyed on InvalidRequestException (bad or deleted agent -> re-register the agent, surface the agent id to the operator) never run; SDK retry policies treat 501 as a non-retryable server-side fault rather than a client validation error; and no NFS/HDFS/SMB location is ever persisted, so every downstream CreateTask/DescribeLocation* flow over those families is unreachable too. The cross-family consistency guarantee (identical code and identical message from DescribeAgent and from each Create*Location) silently does not hold.

Fix: implement the three operation handlers and their AgentArns validation — the handler bodies of the CreateLocationNfs, CreateLocationHdfs and CreateLocationSmb operations in the datasync service crate (each handler is the only file for its operation), resolving each ARN in OnPremConfig.AgentArns / AgentArns against the agent store the DescribeAgent handler already reads, and returning the same modeled InvalidRequestException with the same message builder DescribeAgent uses for an unknown agent, before creating any location. On success the handler must persist the location and return a LocationArn matching the modeled location/loc-[0-9a-z]{17} pattern.

Traces: datasync.agent-not-found-error-is-consistent-across-location-types (assertion at the CreateLocationNfs call; the CreateLocationHdfs and CreateLocationSmb assertions that follow it in the same check are blocked behind it). Handler files: services/service-com-amazonaws-datasync-fmrsservice/src/ops/create_location_nfs.rs:173 (and create_location_hdfs.rs, create_location_smb.rs — both still carrying the //! SCAFFOLDED, NOT IMPLEMENTED: marker).

No open issue covers these three operations: the open datasync issues concern CreateLocationEfs validation, CreateLocationS3 role diagnosis, task reads/executions, TagResource and DeleteLocation; the resolved stub issues cover CreateLocationAzureBlob, CreateLocationEfs and CreateLocationObjectStorage only.

## Evidence
Expected (real AWS, per the recorded run and the Smithy model):
  DescribeAgent(AgentArn=arn:aws:datasync:<region>:<account>:agent/agent-<id>)
    -> HTTP 400, Code=InvalidRequestException, Message="agent-<id> does not exist or has been deleted."
  CreateLocationNfs(Subdirectory="/export", ServerHostname=<host>, OnPremConfig={AgentArns:[<same arn>]})
    -> HTTP 400, Code=InvalidRequestException, Message="agent-<id> does not exist or has been deleted." (identical to DescribeAgent)
  CreateLocationHdfs(..., AgentArns=[<same arn>]) and CreateLocationSmb(..., AgentArns=[<same arn>]) -> same code, same message.
  Control: CreateLocationObjectStorage with no AgentArns succeeds and DescribeLocationObjectStorage echoes the LocationArn.

Actual (emulator):
  DescribeAgent -> correct: InvalidRequestException with "<agent-id> does not exist or has been deleted."
  CreateLocationNfs -> HTTP 501, Code=NotImplemented, Message="operation CreateLocationNfs is scaffolded but not implemented in this emulator"
  CreateLocationHdfs / CreateLocationSmb -> same 501 NotImplemented (handlers still carry the scaffold marker)
  Control (CreateLocationObjectStorage + DescribeLocationObjectStorage) -> passes.

Replay assertion: AssertionError: CreateLocationNfs with the unregistered agent failed with 'NotImplemented', expected 'InvalidRequestException'.
Fixed text vs variable parts of the required message: fixed = " does not exist or has been deleted."; variable = the agent id (`agent-<17 chars>`) parsed from the AgentArn supplied in the request. The region, account and id in any one recording are run values, not part of the requirement.

## Steps to Reproduce
1. Boot the datasync emulator.
2. Build a syntactically valid but unregistered agent ARN: arn:aws:datasync:<region>:<account>:agent/agent-<17 lowercase alphanumerics>.
3. Call DescribeAgent with it and observe the correct refusal: HTTP 400 InvalidRequestException, Message "agent-<id> does not exist or has been deleted.".
4. Call CreateLocationNfs(Subdirectory="/export", ServerHostname=<any host>, OnPremConfig={"AgentArns": [that ARN]}) and observe HTTP 501 with Code NotImplemented instead of the same InvalidRequestException/message.
5. Repeat with CreateLocationHdfs(NameNodes=[{Hostname:<host>,Port:8020}], AuthenticationType="SIMPLE", SimpleUser="hadoop", AgentArns=[that ARN]) and CreateLocationSmb(Subdirectory="/share", ServerHostname=<host>, User="guest", Password=<any>, AgentArns=[that ARN]) — both answer the same 501.
6. Re-replay check datasync.agent-not-found-error-is-consistent-across-location-types to confirm all three now match DescribeAgent's code and message, and that the agentless CreateLocationObjectStorage control still succeeds.
