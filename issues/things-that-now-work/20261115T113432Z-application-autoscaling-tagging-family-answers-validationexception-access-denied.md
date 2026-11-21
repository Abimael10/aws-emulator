Issue-Source: trace-sweep-divergence
Issue-Trace: application-autoscaling-error-tag-resource-not-found
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: application-autoscaling-error-tag-resource-not-found applicationautoscaling-registration-error-envelopes
Issue-Shape-Id: com.amazonaws.applicationautoscaling#AnyScaleFrontendService
Issue-Key: application-autoscaling|ListTagsForResource|4xx->4xx|$.Message
Issue-Key: application-autoscaling|ListTagsForResource|4xx->4xx|$.__type
Issue-Key: application-autoscaling|TagResource|4xx->4xx|$.Message
Issue-Key: application-autoscaling|TagResource|4xx->4xx|$.__type
Issue-Key: application-autoscaling|UntagResource|4xx->4xx|$.Message
Issue-Key: application-autoscaling|UntagResource|4xx->4xx|$.__type
Issue-Commit: 2e402f9443eafd68449b809d69268fcd019d861c

# application-autoscaling: tagging family answers ValidationException "Access Denied" instead of ResourceNotFoundException "Scalable Target not found" for an absent scalable target (3 causes)

> _Note: this issue was identified against the code base at commit `2e402f9443eafd68449b809d69268fcd019d861c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Three net-new wire causes, one per operation of the Application Auto Scaling resource-tagging family. They share a single fault: none of the three handlers resolves the supplied scalable-target ARN against registered-scalable-target state, so a syntactically valid ARN that names no live target is refused as an authorization/validation failure instead of a not-found. The error envelope that comes back (__type 'ValidationException', Message 'Access Denied') is identical for all three, which is why they are bundled.

An open BEHAVIORAL issue describes the same fault from the behavioral suite for the write path only: '20261113T225650Z-application-autoscaling-tagresource-on-a-deregistered-scalable-target.md' (TagResource on a deregistered ARN). It does NOT carry these wire replay keys, and it explicitly records ListTagsForResource on a dead ARN as a passing control — the wire corpus shows that leg diverging too. These causes are therefore filed in full; a fixer should close them together with that issue.

Cause 1 of 3 — ListTagsForResource on an unregistered scalable-target ARN. Fixture application-autoscaling-error-tag-resource-not-found, step task#1 (same cluster also covers applicationautoscaling-registration-error-envelopes#task#8).
AWS: Smithy (com.amazonaws.applicationautoscaling#AnyScaleFrontendService): ListTagsForResource.ResourceARN -> AmazonResourceName, required, @length 1..1011, @pattern ^arn:.+:application-autoscaling:.+:[0-9]+:scalable-target\/[a-zA-Z0-9-]+$; errors = [ResourceNotFoundException (errorKind client, httpError 404)] and nothing else. ResourceNotFoundException doc: 'The specified resource doesn't exist.' The recorded response for an ARN that resolves to no registered target carries __type 'ResourceNotFoundException' and Message 'Scalable Target not found'. That message is entirely FIXED text for this error class — it interpolates no name, id, limit or member from the request or from state. The recorded status for this envelope is 400 and the emulator already answers 400, so the status is not part of this divergence; only the error code and message are.
Emulator: answers 400 with __type 'ValidationException' and Message 'Access Denied'. ValidationException is not even in this operation's modeled error set, and 'Access Denied' is not a message AWS words for any error class on this operation.
Why: a client cannot tell 'the target does not exist' from 'you lack permission / your request is malformed'. Teardown and reconciliation code catches ResourceNotFoundException to treat an absent target as already-clean; an 'Access Denied' ValidationException makes idempotent cleanup abort and surfaces a spurious IAM problem, and botocore's generated ResourceNotFoundException handler for this operation never fires.
Fix: the ListTagsForResource handler's ARN resolution/validation in the application-autoscaling crate — resolve ResourceARN against registered-scalable-target state and raise the modeled ResourceNotFoundException with the fixed message 'Scalable Target not found' on a miss, instead of falling through to a generic ValidationException/'Access Denied' refusal.

Cause 2 of 3 — TagResource on an unregistered scalable-target ARN. Fixture application-autoscaling-error-tag-resource-not-found, step task#2.
AWS: Smithy: TagResource.ResourceARN -> AmazonResourceName, required, same @length/@pattern; TagResource.Tags -> TagMap, required; errors = [ResourceNotFoundException (httpError 404), TooManyTagsException (400), ValidationException (400)]. ValidationException on this operation is reserved for a malformed or unsatisfiable request (an ARN failing the @pattern, a bad tag key/value), not for an absent resource. The recorded response for an absent target is __type 'ResourceNotFoundException', Message 'Scalable Target not found' (fixed text, no interpolated values) at the recorded status 400.
Emulator: 400 with __type 'ValidationException', Message 'Access Denied'.
Why: same as cause 1, plus the refusal masquerades as a permissions failure on a write, so retry wrappers either retry forever or escalate to an operator.
Fix: the TagResource handler's target resolution/validation in the application-autoscaling crate; the refusal must stay non-mutating (a refused TagResource must not revive or create the target).

Cause 3 of 3 — UntagResource on an unregistered scalable-target ARN. Fixture application-autoscaling-error-tag-resource-not-found, step task#3.
AWS: Smithy: UntagResource.ResourceARN -> AmazonResourceName, required, same @length/@pattern; UntagResource.TagKeys -> TagKeyList, required, @length 0..200; errors = [ResourceNotFoundException (httpError 404), ValidationException (400)]. Recorded response for an absent target: __type 'ResourceNotFoundException', Message 'Scalable Target not found' (wholly fixed text) at the recorded status 400.
Emulator: 400 with __type 'ValidationException', Message 'Access Denied'.
Why: same as cause 1; tag cleanup on a torn-down target cannot be made idempotent.
Fix: the UntagResource handler's target resolution/validation in the application-autoscaling crate — the same resolution the other two operations need, so all three should share one lookup helper.

Traces: application-autoscaling-error-tag-resource-not-found#task#1 (ListTagsForResource), #task#2 (TagResource), #task#3 (UntagResource); applicationautoscaling-registration-error-envelopes#task#8 clusters with task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200). 3 net-new divergence clusters, all verdict=mismatch, all service=application-autoscaling, none carrying stub_path or owned_by.

Cause 1 — ListTagsForResource, fixture application-autoscaling-error-tag-resource-not-found, step task#1 (affected_count 2; also applicationautoscaling-registration-error-envelopes#task#8):
  expected_status 400, actual_status 400
  $.__type: value_mismatch (expected 'ResourceNotFoundException', got 'ValidationException')
  $.Message: value_mismatch (expected 'Scalable Target not found', got 'Access Denied')

Cause 2 — TagResource, same fixture, step task#2:
  expected_status 400, actual_status 400
  $.__type: value_mismatch (expected 'ResourceNotFoundException', got 'ValidationException')
  $.Message: value_mismatch (expected 'Scalable Target not found', got 'Access Denied')

Cause 3 — UntagResource, same fixture, step task#3:
  expected_status 400, actual_status 400
  $.__type: value_mismatch (expected 'ResourceNotFoundException', got 'ValidationException')
  $.Message: value_mismatch (expected 'Scalable Target not found', got 'Access Denied')

Requirement vs. observed, for every one of the three operations:
  Expected error code: ResourceNotFoundException. Expected message: 'Scalable Target not found' — 100% fixed text for this error class; it contains no request- or state-derived part (no ARN, resource id, dimension or namespace is interpolated).
  Observed error code: ValidationException. Observed message: 'Access Denied'.
  Status: recorded 400, emulator 400 — matches, so no status change is being asked for here. (Smithy classifies ResourceNotFoundException with httpError 404; the recorded envelope for this corpus is 400 and that part passed, so the fix must not regress the status the recording expects.)

Smithy backing (services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model/com-amazonaws-applicationautoscaling-anyscalefrontendservice.json):
  ListTagsForResource errors = [ResourceNotFoundException 404] — ValidationException is NOT a modeled error for it at all.
  TagResource errors = [ResourceNotFoundException 404, TooManyTagsException 400, ValidationException 400].
  UntagResource errors = [ResourceNotFoundException 404, ValidationException 400].
  ResourceNotFoundException: structure, client error, members {Message: ExceptionMessage, ResourceName: AmazonResourceName}, doc 'The specified resource doesn't exist.'
  ValidationException: doc 'An exception was thrown for a validation issue. Review the available parameters for the API request.' — i.e. a malformed request, not an absent resource.
  All three ResourceARN members share @pattern ^arn:.+:application-autoscaling:.+:[0-9]+:scalable-target\/[a-zA-Z0-9-]+$, which the ARN under test satisfies, so ValidationException is not the applicable class.

Related open issue that does not carry these keys: 20261113T225650Z-application-autoscaling-tagresource-on-a-deregistered-scalable-target.md.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay fixture 'application-autoscaling-error-tag-resource-not-found' step 'task#1' — ListTagsForResource with a well-formed application-autoscaling scalable-target ARN that names no registered target.
   Observed: HTTP 400, body __type='ValidationException', Message='Access Denied'.
   Expected: __type='ResourceNotFoundException', Message='Scalable Target not found' (status as recorded, 400).
4. Cause 2: same fixture, step 'task#2' — TagResource(ResourceARN=<that absent-target ARN>, Tags={...}).
   Observed: 400 ValidationException / 'Access Denied'. Expected: ResourceNotFoundException / 'Scalable Target not found'. The refusal must not create or revive the target.
5. Cause 3: same fixture, step 'task#3' — UntagResource(ResourceARN=<that absent-target ARN>, TagKeys=[...]).
   Observed: 400 ValidationException / 'Access Denied'. Expected: ResourceNotFoundException / 'Scalable Target not found'.
6. Cross-check: fixture 'applicationautoscaling-registration-error-envelopes' step 'task#8' clusters with cause 1 and must change with it.
7. Re-replay all of the above after the fix; every step must report passed, and the registration/tagging steps that pass today must keep passing (positive-path TagResource/UntagResource/ListTagsForResource on a live target are unaffected).
