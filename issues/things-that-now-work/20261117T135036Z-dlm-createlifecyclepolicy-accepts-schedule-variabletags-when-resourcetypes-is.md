Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dlm.create-policy-variable-tags-without-instance-resource-type-rejected
Issue-Shape-Id: com.amazonaws.dlm#dlm_20180112
Issue-Key: dlm.create-policy-variable-tags-without-instance-resource-type-rejected|_client_error#5|AssertionError|raise AssertionError(f"expected a ClientError, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: f8316590bcf97b65fd5e64b3c988acfd983e0aed

# dlm: CreateLifecyclePolicy accepts Schedule.VariableTags when ResourceTypes is VOLUME instead of refusing with InvalidRequestException

> _Note: this issue was identified against the code base at commit `f8316590bcf97b65fd5e64b3c988acfd983e0aed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLifecyclePolicy

## Description
Cause 1 of 1: DLM's CreateLifecyclePolicy does not enforce that Schedule.VariableTags is only valid for instance-targeting EBS snapshot policies.

AWS: Smithy (services/service-com-amazonaws-dlm-dlm-20180112/smithy-model/com-amazonaws-dlm-dlm-20180112.json) documents Schedule.VariableTags as "[AMI policies and snapshot policies that target instances only] ... Variable tags are only valid for EBS Snapshot Management - Instance policies." PolicyDetails.ResourceTypes selects VOLUME or INSTANCE. CreateLifecyclePolicy models InvalidRequestException (client error, HTTP 400) for a request that "has invalid parameters". Recorded behavior: a CreateLifecyclePolicy with PolicyType=EBS_SNAPSHOT_MANAGEMENT, ResourceTypes=["VOLUME"] and a Schedules[0].VariableTags entry is refused with HTTP 400, Error Code "InvalidRequestException" and message "The following parameter(s) are invalid for Resource Type {VOLUME}: VariableTags". In that message the fixed text is "The following parameter(s) are invalid for Resource Type {", "}: "; the braced value is the request's PolicyDetails.ResourceTypes entry and the trailing list is the offending member name(s) from the request (here VariableTags) — neither is a constant to be hard-coded. The identical VariableTags entry is accepted when ResourceTypes is ["INSTANCE"] and reads back verbatim from GetLifecyclePolicy.

Emulator: the VOLUME request SUCCEEDS — HTTP 200 with a PolicyId (e.g. a policy-… id) — so the invalid combination is persisted as a real policy rather than rejected.

Why: a client that relies on AWS refusing this combination (and on catching InvalidRequestException to correct ResourceTypes or drop VariableTags) sees a success against the emulator, creates a policy AWS would never have created, and its negative-path/validation tests pass vacuously. State then diverges too: the emulator holds a VOLUME policy carrying VariableTags, which GetLifecyclePolicy/GetLifecyclePolicies will report and which cannot exist in AWS.

Fix: the CreateLifecyclePolicy handler's request validation in the DLM service crate — cross-member validation of PolicyDetails (ResourceTypes vs each Schedule's instance-only members, VariableTags in particular) must reject with InvalidRequestException/400 before any policy is allocated or stored. The same rule belongs on UpdateLifecyclePolicy's validation path so an accepted policy cannot be mutated into the invalid shape.

Traces: dlm.create-policy-variable-tags-without-instance-resource-type-rejected.

## Evidence
Check dlm.create-policy-variable-tags-without-instance-resource-type-rejected.

Expected (AWS): CreateLifecyclePolicy(ExecutionRoleArn=<dlm role>, Description=<desc>, State=ENABLED, PolicyDetails={PolicyType: EBS_SNAPSHOT_MANAGEMENT, ResourceTypes: ["VOLUME"], TargetTags: [...], Schedules: [{Name, CreateRule, RetainRule, VariableTags: [{Key: ..., Value: "$(instance-id)"}]}]}) is refused:
  HTTP 400
  Error.Code    = "InvalidRequestException"
  Error.Message = "The following parameter(s) are invalid for Resource Type {VOLUME}: VariableTags"
and no policy is created.

Actual (emulator): the call returns HTTP 200:
  {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'PolicyId': 'policy-0000000000000001'}
The check's helper raises: AssertionError: expected a ClientError, but the call SUCCEEDED and returned {... 'PolicyId': 'policy-0000000000000001'}.

Positive control in the same check (ResourceTypes=["INSTANCE"] with the identical VariableTags entry accepted and read back by GetLifecyclePolicy) must keep passing after the fix.

## Steps to Reproduce
1. Create an IAM role trusted by dlm.amazonaws.com (any ARN matching the ExecutionRoleArn pattern ^arn:aws(-[a-z]{1,3}){0,2}:iam::\d+:role/).
2. Call dlm.CreateLifecyclePolicy with State=ENABLED, a Description, and PolicyDetails = {PolicyType: "EBS_SNAPSHOT_MANAGEMENT", ResourceTypes: ["VOLUME"], TargetTags: [{Key: <tag>, Value: "variable-tags-check"}], Schedules: [{Name: "sim2real-sched", CreateRule: {Interval: 24, IntervalUnit: "HOURS", Times: ["03:00"]}, RetainRule: {Count: 1}, VariableTags: [{Key: <tag>, Value: "$(instance-id)"}]}]}.
3. Observe HTTP 200 and a PolicyId; AWS returns HTTP 400 InvalidRequestException "The following parameter(s) are invalid for Resource Type {VOLUME}: VariableTags".
4. Re-run the identical request with ResourceTypes: ["INSTANCE"] — it must still be accepted and GetLifecyclePolicy must read the VariableTags entry back unchanged.
