Issue-Source: trace-sweep-divergence
Issue-Trace: drs-tag-resource-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-tag-resource-lifecycle
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|ListTagsForResource|2xx->2xx|$.tags.extra
Issue-Key: drs|ListTagsForResource|4xx->4xx|$.message
Issue-Key: drs|ListTagsForResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: drs|ListTagsForResource|4xx->4xx|status
Issue-Commit: 20b40f42cd78b95666b130bae2475dd4b1dad646

# drs tag APIs: ListTagsForResource drops TagResource-added keys and rejects an unsupported resource ARN as 404 instead of 400

> _Note: this issue was identified against the code base at commit `20b40f42cd78b95666b130bae2475dd4b1dad646`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Two net-new root causes in the drs resource-tagging surface (GET/POST/DELETE /tags/{resourceArn}). Both were observed on ListTagsForResource and both must be closed.

Cause 1 of 2: tags written through TagResource are not visible to ListTagsForResource (fixture drs-tag-resource-lifecycle, step task#6).
AWS: Smithy (com.amazonaws.drs) TagResource — POST /tags/{resourceArn}, input {resourceArn: ARN @httpLabel @required, tags: TagsMap @required}, output Unit, @idempotent; ListTagsForResource — GET /tags/{resourceArn}, output {tags: TagsMap}. TagsMap is a map<TagKey,TagValue> (each 0..256 chars). AWS semantics are that TagResource MERGES the supplied keys into the resource's tag set and a subsequent ListTagsForResource on the same resourceArn returns 200 with the full merged map, including every key just added.
Emulator: ListTagsForResource answers 200 but the returned map is missing the key added by the preceding TagResource call — replay reports `$.tags.extra: missing (expected 'v', got None)`; the rest of the map matches. So the tag write is accepted (or swallowed) without reaching the state the read serves.
Why: a client that tags a DRS resource and then reads its tags back sees its own write disappear. Tag-driven workflows (cost allocation, tag-based IAM/attribute conditions, idempotent re-tagging, and any read-after-write assertion in IaC such as CloudFormation/Terraform drift detection) conclude the tag was never applied and either retag in a loop or report drift forever. A delete-then-verify flow can likewise never confirm its own change.
Fix: the TagResource handler's state mutation and/or the shared tag store that ListTagsForResource reads — the write must merge into the same per-resource tag map keyed by resourceArn that the read serves. Note the open issue "Implement drs TagResource: unimplemented scaffold stub" (20261110T042638Z-drs-tagresource-stub.md) covers TagResource being a scaffold; this cause is the ListTagsForResource read-back divergence the replay reports in its own right and is not marked as owned by that issue, so it must be verified independently once the write path exists.
Traces: drs-tag-resource-lifecycle#task#6.

Cause 2 of 2: an unsupported/unparseable resource ARN is refused as ResourceNotFoundException (404) instead of ValidationException (400) (fixture drs-tag-resource-lifecycle, step task#10).
AWS: Smithy ListTagsForResource models errors ValidationException (httpError 400), ResourceNotFoundException (404), AccessDeniedException (403), ThrottlingException (429), InternalServerException (500); resourceArn carries @length(20,2048) and @pattern "^arn:.{16,2044}$". When the ARN is syntactically acceptable to the label but names a resource type DRS tagging does not support, AWS refuses it as an INPUT fault: HTTP 400, error type ValidationException (x-amzn-errortype: ValidationException), body message exactly "Invalid input resource arn" — that message is entirely fixed text, with no value interpolated from the request or from state. The recorded response is the source for the status and message.
Emulator: HTTP 404 with x-amzn-errortype: ResourceNotFoundException and message "Resource with Arn: '<arn>' was not found" (the recorded run shows the launch-configuration-template ARN interpolated; the ARN value itself is environment-specific and is not the requirement — the wrong status, wrong error shape and wrong message template are).
Why: the fault class is inverted. A 404/ResourceNotFoundException tells a client the resource merely does not exist yet, which is a retryable/creatable condition; SDK retry and waiter logic and provisioning code will poll or proceed, whereas AWS's 400/ValidationException is terminal and tells the caller its ARN is malformed or of an untaggable type. Error-code-based branching (catching ValidationException to surface a user input error) never fires.
Fix: resourceArn parsing/validation in the tagging handlers (ListTagsForResource, and the same path shared with TagResource/UntagResource) must run BEFORE the resource lookup: reject an ARN whose resource type is not a taggable DRS resource with ValidationException/400 and the fixed message "Invalid input resource arn", and reserve ResourceNotFoundException/404 for a well-formed ARN of a supported type that is absent from state.
Traces: drs-tag-resource-lifecycle#task#10.

## Evidence
Cause 1 — fixture drs-tag-resource-lifecycle step task#6, ListTagsForResource: expected 200, got 200, body diff `$.tags.extra: missing (expected 'v', got None)`.

Cause 2 — fixture drs-tag-resource-lifecycle step task#10, ListTagsForResource: expected status 400, got 404; `$.message: value_mismatch (expected 'Invalid input resource arn', got "Resource with Arn: 'arn:aws:drs:us-west-2:000000000000:launch-configuration-template/lct-00000000000000abc' was not found")`; `$headers.x-amzn-errortype: shape_mismatch (expected <shaped ValidationException>, got 'ResourceNotFoundException')`.

Model evidence: Smithy com.amazonaws.drs — ListTagsForResource GET /tags/{resourceArn} -> {tags: TagsMap}, errors include ValidationException(400) and ResourceNotFoundException(404); TagResource POST /tags/{resourceArn} with required tags: TagsMap, output Unit, @idempotent; resourceArn @length(20,2048) @pattern ^arn:.{16,2044}$.

## Steps to Reproduce
Replay fixture drs-tag-resource-lifecycle against the emulator.

Cause 1: at step task#6 — create/obtain a taggable DRS resource, call TagResource on its ARN with a tag map containing a new key (e.g. extra=v), then call ListTagsForResource (GET /tags/{resourceArn}) on the same ARN. Expect 200 with the merged map including the newly added key; observe the key absent.

Cause 2: at step task#10 — call ListTagsForResource (GET /tags/{resourceArn}) with an ARN of a resource type DRS tagging does not support (the fixture uses a launch-configuration-template ARN). Expect HTTP 400, x-amzn-errortype: ValidationException, body message "Invalid input resource arn"; observe HTTP 404 with ResourceNotFoundException and a "Resource with Arn: '...' was not found" message.
