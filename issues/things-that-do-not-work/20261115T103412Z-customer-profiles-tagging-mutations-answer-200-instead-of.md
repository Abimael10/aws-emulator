Issue-Source: trace-sweep-divergence
Issue-Trace: customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal
Issue-Shape-Id: com.amazonaws.customerprofiles#CustomerProfiles_20200815
Issue-Key: customer-profiles|TagResource|2xx->2xx|status
Issue-Key: customer-profiles|UntagResource|2xx->2xx|status
Issue-Commit: f40222b80bff0bb01bc5725930758a9790cd3d69

# customer-profiles tagging mutations answer 200 instead of the 204 empty-body success

> _Note: this issue was identified against the code base at commit `f40222b80bff0bb01bc5725930758a9790cd3d69`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Two net-new wire causes sharing one fault: the two Customer Profiles tagging MUTATIONS (TagResource, UntagResource) now reach the tag store and apply the change, but they project their empty-output success as HTTP 200 instead of the 204-with-no-body that AWS returns. The body comparison passes on both steps; only the status line diverges, so the shared defect is the success-status projection for these empty-output REST-JSON operations, not the tag state itself.

Cause 1 of 2: TagResource returns 200 where AWS returns 204.
AWS: Smithy: TagResource is POST /tags/{resourceArn}; resourceArn is a required @httpLabel targeting TagArn (@length 0..256), tags is a required TagMap (@length 1..50); the output shape TagResourceResponse is EMPTY; modeled errors BadRequestException/400, ResourceNotFoundException/404, InternalServerException/500. The model carries the protocol-default success code 200, but recorded behavior shows the service answering 204 with no body for this empty-output operation - the recording is authoritative for the wire here. Recorded response: HTTP 204, no body, and the submitted tags unioned onto the resource's existing tag set.
Emulator: HTTP 200 (with an empty/OK body) for the same well-formed request.
Why: a client that branches on the exact success status - an SDK or HTTP layer that treats 204 as 'no content, do not parse' and 200 as 'a body is present and must be parsed/validated', a strict contract test, or a proxy/cache keyed on 204 - sees the wrong outcome class for a call that in fact succeeded. Anything asserting the documented 204 on a successful tag write fails against the emulator while the tag was actually applied, which is the hardest kind of mismatch to diagnose.
Fix: the TagResource handler's success projection (the response-status mapping for empty-output operations in the customer-profiles REST-JSON serializer) - emit 204 with no body on success and keep the modeled 400/404/500 error statuses unchanged.
Traces: customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal#task#2.

Cause 2 of 2: UntagResource returns 200 where AWS returns 204.
AWS: Smithy: UntagResource is DELETE /tags/{resourceArn}; resourceArn is the same required @httpLabel TagArn, tagKeys is a required @httpQuery list (@length 1..50); the output shape UntagResourceResponse is EMPTY; same modeled errors (400/404/500). Recorded behavior: HTTP 204 with no body, with ONLY the named keys removed and the remaining keys left in place.
Emulator: HTTP 200 for the same well-formed removal.
Why: same class of breakage as Cause 1, on the teardown half of the tagging round-trip: a caller that untags during cleanup and asserts 204 reads the removal as a differently-shaped success, and a layer that only skips body parsing on 204 will try to interpret a response AWS never bodies.
Fix: the UntagResource handler's success projection, the same empty-output status mapping as Cause 1 - 204, no body.
Traces: customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal#task#5.

Related but not covering: open issue 20261103T055557Z-customer-profiles-tagging-surface-unusable-tag-untag-listtags.md recorded these two operations when they answered 400 (its Causes 2 and 3, keys customer-profiles|TagResource|2xx->4xx|status and customer-profiles|UntagResource|2xx->4xx|status). The ARN refusal is gone - both calls now succeed - so that issue's recorded divergence no longer reproduces, and the replay does not attribute these clusters to it (owned_by empty). What remains is strictly the success-status projection reported here.

## Evidence
Replay of simparity-wire-aws-v1 (emulator_up=true, bootstrap_signal=false, score_pct=81.2). Both clusters are verdict=mismatch with the status line as the ONLY diverging key.

Cause 1 - fixture customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal, step task#2, operation TagResource:
  detail: "status: expected 204, got 200"
  expected_status=204, actual_status=200, affected_count=1, stub_path empty, owned_by empty.
  divergence_key: service=customer-profiles|TagResource|status=204->200|keys=status

Cause 2 - same fixture, step task#5, operation UntagResource:
  detail: "status: expected 204, got 200"
  expected_status=204, actual_status=200, affected_count=1, stub_path empty, owned_by empty.
  divergence_key: service=customer-profiles|UntagResource|status=204->200|keys=status

No body key diverged on either step, so the tag union (TagResource) and the key-scoped removal (UntagResource) are behaving; the requirement still unmet is the empty-body 204 success status. The ARN, tag keys and tag values in the recording are that run's values and are not part of the requirement.

## Steps to Reproduce
Let D be an existing Customer Profiles domain and ARN = 'arn:<partition>:profile:<region>:<account>:domains/D'.

Cause 1: POST /tags/<ARN> (TagResource) with a valid tags map, e.g. tags={'stage':'probe'} on a domain that already carries tags. Expect HTTP 204 and an empty body. Observe HTTP 200.

Cause 2: DELETE /tags/<ARN>?tagKeys=stage (UntagResource) naming a key that exists on the resource. Expect HTTP 204 and an empty body, with the resource's other keys still present. Observe HTTP 200.

Re-replay fixture customer-profiles-tag-resource-after-create-unions-and-untag-resource-scopes-the-removal (steps task#2 and task#5) to confirm both statuses are 204.
