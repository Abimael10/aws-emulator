Issue-Source: trace-sweep-divergence
Issue-Trace: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters
Issue-Step: task#2
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters quicksight-data-source-lifecycle-create-update-delete quicksight-folder-lifecycle-and-duplicate-rejected
Issue-Shape-Id: com.amazonaws.quicksight#QuickSight_20180401
Issue-Key: quicksight|DescribeDataSource|4xx->4xx|$.RequestId
Issue-Key: quicksight|CreateFolder|4xx->4xx|$.RequestId
Issue-Commit: 8dee68d35c2160346398ed08e3fbdb4f57e425bf

# QuickSight DescribeDataSource 404 and CreateFolder 409 error bodies emit a synthetic RequestId member AWS leaves out

> _Note: this issue was identified against the code base at commit `8dee68d35c2160346398ed08e3fbdb4f57e425bf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDataSource

## Description
The emulator's QuickSight error-response serializer fills the modeled `RequestId` member of the error payload for these two error classes with a synthetic counter-derived UUID. For the recorded error classes AWS QuickSight returns the request id in the `x-amzn-RequestId` response HEADER and leaves the member out of the JSON body, so the emulator's body is shape-divergent from AWS. The divergence is the PRESENCE of the member, not its value: the recordings assert `RequestId` absent, so it reproduces on every run even though the emulator's id is deterministic. On both steps the HTTP status, the `x-amzn-errortype` header and the `Message` text MATCH the recording - the extra member is the only divergence. This issue bundles 2 causes - one shared serializer fault observed on two operations and two error classes - and BOTH must be closed and re-replayed.

Cause 1 of 2: DescribeDataSource on a data source that is not present - 404 ResourceNotFoundException. Fixture `quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters`, step `task#2` (the same cluster also covers fixture `quicksight-data-source-lifecycle-create-update-delete` step `task#9`, the post-delete describe).
AWS: Smithy (quicksight-20180401): DescribeDataSource (GET /accounts/{AwsAccountId}/data-sources/{DataSourceId}) models ResourceNotFoundException (errorKind client, httpError 404); that shape's members are Message, ResourceType and RequestId, all OPTIONAL, and RequestId's doc is "The Amazon Web Services request ID for this request." Recorded behavior: the 404 body carries only the data-source not-found message - its fixed text is AWS's ARN-based ResourceNotFoundException wording for a data source, with the data source's ARN taken from the request and the emulator's state (account, region, data-source id) - and NO `RequestId` member; the id travels in the response header. Because the member is optional in the model, emitting it is not required by the model, and the recording shows AWS omitting it for this error class.
Emulator: returns the correct 404, the correct x-amzn-errortype and the correct message, but the JSON body additionally contains a top-level `RequestId` holding a synthetic sequential UUID (replay: "$.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000002')"), a member the recording does not have.
Why: a client or SDK contract test that snapshots or strictly diffs a QuickSight error body sees a member AWS never sends on the most commonly asserted error class (not-found), and code that reads the request id out of the body for support correlation works against the emulator and silently yields nothing against AWS, where the id is header-only.
Fix: the quicksight error-response serializer - the shared error-payload builder that populates modeled exception members before writing the body. For this error class it must leave `RequestId` unset in the serialized body and surface the id only through the `x-amzn-RequestId` response header, instead of defaulting the modeled member from the request-id allocator. No handler change is needed: the handlers already produce the right status, error type and message.
Traces: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters#task#2 (cluster also: quicksight-data-source-lifecycle-create-update-delete#task#9).

Cause 2 of 2: CreateFolder re-creating a folder id that already exists - 409. Fixture `quicksight-folder-lifecycle-and-duplicate-rejected`, step `task#5`.
AWS: Smithy: CreateFolder (POST /accounts/{AwsAccountId}/folders/{FolderId}) models ResourceExistsException (errorKind client, httpError 409) alongside ConflictException and LimitExceededException at the same status; ResourceExistsException's members are Message, ResourceType and RequestId, all OPTIONAL, with the same RequestId doc. Recorded behavior: the 409 body carries the already-exists message - its fixed text is AWS's wording for a folder that already exists, with the folder's identifier/ARN filled from the request and the emulator's state - and NO `RequestId` member. The emulator already matches the recorded 409, error type header and message on this step, so nothing about the message template is in question here, only the extra member.
Emulator: returns the correct 409 and message plus the same stray top-level `RequestId` synthetic UUID (replay: "$.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000005')").
Why: same as Cause 1 - the duplicate-create error body is shape-divergent from AWS, so strict error-payload comparisons fail on an error that is otherwise exactly right.
Fix: the same quicksight error-response serializer; this second cause confirms the behavior spans operations and error classes (404 ResourceNotFoundException and 409 ResourceExistsException), so a per-handler patch would leave the other QuickSight error classes diverging.
Traces: quicksight-folder-lifecycle-and-duplicate-rejected#task#5.

Related issues that do NOT cover these two clusters:
- 20261112T051305Z-quicksight-error-payloads-carry-a-synthetic-requestid-member.md is marked FIXED and its replay keys were exactly quicksight|DescribeDataSource|4xx->4xx|$.RequestId and quicksight|CreateFolder|4xx->4xx|$.RequestId - the same two clusters filed here. The divergence still reproduces on the current checkout and the replay reports both clusters as unowned, so that fix did not hold (or was closed without re-replaying these steps). A resolved issue does not cover a live failure.
- 20261120T210750Z-quicksight-createdatasource-400-and-describefolder-404-bodies-emit-a.md (open) owns the SAME serializer behavior but its replay key is bound to the CreateDataSource 400 and DescribeFolder 404 clusters only; the replay confirms it does not own these two.
- 20261123T123628Z-quicksight-error-bodies-omit-the-modeled-requestid-member.md (open) reports the OPPOSITE direction for CreateGroup 409, DescribeGroup 404 and DescribeIAMPolicyAssignment 404, where the recordings assert the member PRESENT. Taken together the correct behavior is per-error-class as RECORDED, so the fix must make the serializer honor the recorded member set per error class rather than flipping a single global flag in either direction - flipping it globally will close one of these issues by breaking the other.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout (emulator_up true, /__admin/health answered 200 and the data listener accepts). 49 steps replayed: 37 passed, 12 mismatched, 0 errored, 0 blocked; 6 divergence clusters, 4 of which are owned by existing open issues and are not filed here.

Cause 1 - fixture `quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters`, step `task#2`, operation DescribeDataSource, verdict mismatch, expected_status 404 / actual_status 404:
  $.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000002')
No other key diverged - status, $headers.x-amzn-errortype and $.Message all matched the recorded ResourceNotFoundException. Cluster size 2; other member: quicksight-data-source-lifecycle-create-update-delete#task#9 (describe after delete, same 404 body shape).

Cause 2 - fixture `quicksight-folder-lifecycle-and-duplicate-rejected`, step `task#5`, operation CreateFolder, verdict mismatch, expected_status 409 / actual_status 409:
  $.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000005')
No other key diverged.

Expected (AWS, per recording + Smithy): error body = {"Message": "<that error class's template filled from the request and the emulator's state>"} (plus ResourceType where the service sets it), with the request id only in the x-amzn-RequestId response header.
Actual (emulator): the same status, x-amzn-errortype and Message, plus a top-level `RequestId` member holding a synthetic sequential UUID.

Model references used: DescribeDataSource - GET /accounts/{AwsAccountId}/data-sources/{DataSourceId}, errors include ResourceNotFoundException (client, httpError 404); CreateFolder - POST /accounts/{AwsAccountId}/folders/{FolderId}, errors include ResourceExistsException (client, httpError 409) and ConflictException (client, 409). ResourceNotFoundException members {Message, ResourceType, RequestId}; ResourceExistsException members {Message, ResourceType, RequestId}; RequestId documented "The Amazon Web Services request ID for this request." All three members are optional, so the model does not require emitting RequestId, and these recordings show it absent.

The divergence is the member's PRESENCE only, independent of the generated id, so it reproduces on every run; the synthetic UUID values quoted above are this run's values and are not part of the requirement.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture `quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters` step `task#2` - GET /accounts/<account>/data-sources/<a data source that is not present>. The emulator answers 404 ResourceNotFoundException with the right x-amzn-errortype and message; diff the JSON body member set against the recording and observe the extra top-level `RequestId` member (the recording has none). The same cluster reproduces on fixture `quicksight-data-source-lifecycle-create-update-delete` step `task#9` (describe after delete).
3. Cause 2: replay fixture `quicksight-folder-lifecycle-and-duplicate-rejected` step `task#5` - POST /accounts/<account>/folders/<an existing folder id> re-creating a folder that already exists. The emulator answers 409 with the right error type and message plus the same extra top-level `RequestId` member.
4. In both cases compare only the body MEMBER SET: the divergence is the presence of `RequestId`, independent of its value, so it reproduces on every run. The request id should appear only in the x-amzn-RequestId response header for these error classes.
5. Re-replay BOTH steps after the serializer fix - closing one does not exercise the other. Also re-replay the CreateGroup 409 / DescribeGroup 404 / DescribeIAMPolicyAssignment 404 steps from 20261123T123628Z, which require RequestId to be PRESENT, to confirm the serializer honors the recorded per-error-class member set rather than a global flag.
