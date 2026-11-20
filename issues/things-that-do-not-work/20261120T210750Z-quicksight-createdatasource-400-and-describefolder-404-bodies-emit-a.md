Issue-Source: trace-sweep-divergence
Issue-Trace: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters quicksight-folder-lifecycle-and-duplicate-rejected
Issue-Shape-Id: com.amazonaws.quicksight#QuickSight_20180401
Issue-Key: quicksight|CreateDataSource|4xx->4xx|$.RequestId
Issue-Key: quicksight|DescribeFolder|4xx->4xx|$.RequestId
Issue-Commit: f85f98f2ce22a299384412ddd2d0ac00fcbe668d

# CreateDataSource 400 and DescribeFolder 404 bodies emit a synthetic RequestId member AWS leaves out

> _Note: this issue was identified against the code base at commit `f85f98f2ce22a299384412ddd2d0ac00fcbe668d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDataSource

## Description
The emulator's QuickSight error serializer unconditionally fills the modeled `RequestId` member of the error payload with a synthetic counter-derived UUID. AWS QuickSight returns the request id in the `x-amzn-RequestId` response HEADER for these error classes and leaves the member out of the JSON body. The divergence is the presence of the member, not its value: the recordings assert `RequestId` absent, so it reproduces on every run even though the emulator's id is deterministic. This issue bundles 2 causes - two operations and two error classes whose status, x-amzn-errortype header and message all MATCH the recording - and both must be closed.

Cause 1 of 2: CreateDataSource rejected for an invalid connection parameter - 400 InvalidParameterValueException.
AWS: Smithy (quicksight-20180401): CreateDataSource models InvalidParameterValueException (errorKind client, httpError 400); the shape's members are Message and RequestId, both OPTIONAL, and RequestId's doc is "The Amazon Web Services request ID for this request." Recorded behavior: the 400 body carries only the InvalidParameterValueException message for the rejected connection parameter and NO RequestId member; the id travels in the response header. The emulator already matches the recorded status, error type header and message text on this step, so nothing about the message template is in question here - only the extra member.
Emulator: returns the correct 400, error type and message, but the JSON body additionally contains a top-level `RequestId` holding a synthetic sequential UUID (replay: "$.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000001')"), a member the recording does not have.
Why: a client or SDK contract test that snapshots or strictly diffs a QuickSight error body sees a member AWS never sends, and code that reads the request id out of the body for support correlation works against the emulator and silently yields nothing against AWS, where the id is header-only.
Fix: the quicksight error-response serializer - the shared error-payload builder that populates modeled exception members before writing the body. It must leave `RequestId` unset in the serialized body and surface the id only through the `x-amzn-RequestId` response header, instead of defaulting the modeled member from the request-id allocator. No handler change is needed: the handlers already produce the right status, error type and message.
Traces: quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters#task#1.

Cause 2 of 2: DescribeFolder on a folder that is not present - 404 ResourceNotFoundException.
AWS: Smithy: DescribeFolder (GET /accounts/{AwsAccountId}/folders/{FolderId}) models ResourceNotFoundException (errorKind client, httpError 404); shape members Message, ResourceType and RequestId, all OPTIONAL. Recorded behavior: the 404 body carries the folder not-found message (fixed text plus the folder's identifier taken from the request/state) and NO RequestId member. The emulator matched status, error type header and message on this step.
Emulator: returns the correct 404 and message plus the same stray top-level `RequestId` synthetic UUID (replay: "$.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000007')").
Why: same as Cause 1 - the folder error body is shape-divergent from AWS, so strict error-payload comparisons fail on an error that is otherwise exactly right.
Fix: the same quicksight error-response serializer; this cause confirms the behavior is serializer-wide rather than per-operation, so a per-handler patch would leave the other QuickSight error classes diverging.
Traces: quicksight-folder-lifecycle-and-duplicate-rejected#task#7.

Related open issue that does NOT cover these two clusters: 20261112T051305Z-quicksight-error-payloads-carry-a-synthetic-requestid-member.md owns the same serializer behavior but its replay key is bound to the DescribeDataSource 404 and CreateFolder 409 clusters only (keys quicksight|DescribeDataSource|4xx->4xx|$.RequestId and quicksight|CreateFolder|4xx->4xx|$.RequestId). The CreateDataSource 400 and DescribeFolder 404 clusters filed here are unowned and would otherwise go unverified; they share the root cause, so a serializer fix should close all of them, but these two steps must be re-replayed to prove it.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout (emulator_up true, /__admin/health 200). 49 steps replayed, 12 mismatched.

Cause 1 - fixture `quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters`, step `task#1`, operation CreateDataSource, verdict mismatch, expected_status 400 / actual_status 400:
  $.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000001')
No other key diverged - status, $headers.x-amzn-errortype and $.Message all matched the recorded InvalidParameterValueException.

Cause 2 - fixture `quicksight-folder-lifecycle-and-duplicate-rejected`, step `task#7`, operation DescribeFolder, verdict mismatch, expected_status 404 / actual_status 404:
  $.RequestId: type_mismatch (expected None, got '00000000-0000-4000-8000-000000000007')
No other key diverged.

Expected (AWS, per recording + Smithy): error body = {"Message": "<the error class's template filled from request/state>"} (plus ResourceType where the service sets it), with the request id only in the x-amzn-RequestId response header.
Actual (emulator): the same body plus a top-level `RequestId` member holding a synthetic sequential UUID.

Model references used: InvalidParameterValueException - errorKind client, httpError 400, members {Message, RequestId} (both optional); ResourceNotFoundException - errorKind client, httpError 404, members {Message, ResourceType, RequestId} (all optional); CreateDataSource errors include InvalidParameterValueException, DescribeFolder errors include ResourceNotFoundException. Because RequestId is optional in both shapes, emitting it is not required by the model, and the recordings show AWS omitting it.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture `quicksight-data-source-creation-rejected-unroutable-host-and-mismatched-parameters` step `task#1` - POST /accounts/<account>/data-sources with connection parameters AWS rejects. The emulator answers 400 with the right error type and message; diff the JSON body against the recording and observe the extra top-level `RequestId` member (the recording has none).
3. Cause 2: replay fixture `quicksight-folder-lifecycle-and-duplicate-rejected` step `task#7` - GET /accounts/<account>/folders/<a folder that is not present>. The emulator answers 404 ResourceNotFoundException with the right message plus the same extra top-level `RequestId` member.
4. In both cases compare only the body MEMBER SET: the divergence is the presence of `RequestId`, independent of its value, so it reproduces on every run. The request id should appear only in the x-amzn-RequestId response header.
5. Re-replay both steps after the serializer fix - closing one does not exercise the other.
