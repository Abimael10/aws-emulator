Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__s3.vpc-flow-log-naming-both-a-log-group-and-a-bucket-rejected
Issue-Key: ec2__s3.vpc-flow-log-naming-both-a-log-group-and-a-bucket-rejected|run#7|AssertionError|assert message == "Please only provide LogGroupName or only provide LogDestination.", (
Issue-Commit: 8ff3b21e7fb9cd785286bd581017af494993989c

# ec2 CreateFlowLogs: the InvalidParameter refusal for a request naming both LogGroupName and an S3 LogDestination carries an emulator-authored message

> _Note: this issue was identified against the code base at commit `8ff3b21e7fb9cd785286bd581017af494993989c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFlowLogs

## Description
Cause 1 of 1: CreateFlowLogs refuses a request that names both LogGroupName and an S3 LogDestination with the right error code but the emulator's own sentence.

AWS: Smithy: CreateFlowLogs (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) accepts LogGroupName — documented as "valid only if the destination type is cloud-watch-logs" — alongside LogDestination and LogDestinationType, and declares "errors": [] (the ec2Query protocol carries refusals out of band), so the status, code and text are recorded behavior only: the call is refused with error code `InvalidParameter` and the message `Please only provide LogGroupName or only provide LogDestination.` That whole sentence is fixed text — it names no member value, id or limit, so there are no variable parts to fill from the request or state.

Emulator: refuses with the correct code `InvalidParameter`, but with the message `LogGroupName is not supported when LogDestinationType is s3` — emulator-authored prose that re-words the rule and (unlike AWS's text) pins the conflict on LogDestinationType rather than on supplying two destinations.

Why: an error message is part of the contract. A caller that matches AWS's sentence to decide which of the two destination members to drop and retry never matches, and an operator or log scraper reading the surfaced text sees wording AWS never emits — so a fixture or runbook written against AWS diverges even though the code is right.

Fix: the destination-exclusivity validation in the EC2 CreateFlowLogs handler — its refusal message, which must be AWS's fixed sentence for this conflict class.

Relation to what is already filed: open issue 20261103T021956Z-ec2-s3-vpc-flow-logs-unknown-filter-and-parameter-refusals.md (cause 4 of 7) carries this check's earlier replay key, the CODE assertion `assert code == "InvalidParameter"` when the emulator answered InvalidParameterCombination. The code is now InvalidParameter, so the replay advances past that assertion and fails on the message instead; the suite does not mark this failure OWNED and that issue's key does not cover the message divergence.

Traces: ec2__s3.vpc-flow-log-naming-both-a-log-group-and-a-bucket-rejected (its positive control — a bucket-only flow log reading back LogDestination — is not reached because the message assertion precedes it).

## Evidence
Failing check (suite heading `### ec2__s3`, marked neither ALREADY FILED nor OWNED):

[mismatch] ec2__s3.vpc-flow-log-naming-both-a-log-group-and-a-bucket-rejected
  run -> assert message == "Please only provide LogGroupName or only provide LogDestination."
  AssertionError: unexpected message for LogGroupName + LogDestination: 'LogGroupName is not supported when LogDestinationType is s3'

Request replayed by the check: CreateFlowLogs(ResourceIds=[<vpc>], ResourceType='VPC', TrafficType='ALL', LogDestinationType='s3', LogDestination='arn:aws:s3:::<bucket>', LogGroupName='<group-name>').

Expected (AWS): error code 'InvalidParameter', message 'Please only provide LogGroupName or only provide LogDestination.' (fixed text in full; no variable parts).
Actual (emulator): error code 'InvalidParameter' (correct), message 'LogGroupName is not supported when LogDestinationType is s3'.

Model evidence: smithy_query(query="operation", operation="CreateFlowLogs") -> input accepts LogGroupName, LogDestination, LogDestinationType; "errors": []. smithy_query(query="doc", shape="CreateFlowLogsRequest", member="LogGroupName") -> "This parameter is valid only if the destination type is cloud-watch-logs" — the rule the refusal enforces; the model fixes neither code nor text, so both come from the recording.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite; the check appears under `### ec2__s3`, unmarked.
2. Standalone repro: create a VPC and an S3 bucket, then call CreateFlowLogs(ResourceIds=[<vpc-id>], ResourceType='VPC', TrafficType='ALL', LogDestinationType='s3', LogDestination='arn:aws:s3:::<bucket>', LogGroupName='<any-group-name>').
3. Read the ClientError: the emulator answers ('InvalidParameter', 'LogGroupName is not supported when LogDestinationType is s3'); AWS answers ('InvalidParameter', 'Please only provide LogGroupName or only provide LogDestination.').
4. Confirm the refusal left no flow log: DescribeFlowLogs(Filters=[{Name:'resource-id',Values:[<vpc-id>]}]) must be empty, and the bucket-only control CreateFlowLogs (same call without LogGroupName) must read back LogDestination unchanged.
5. Re-replay ec2__s3.vpc-flow-log-naming-both-a-log-group-and-a-bucket-rejected to confirm a fix.
