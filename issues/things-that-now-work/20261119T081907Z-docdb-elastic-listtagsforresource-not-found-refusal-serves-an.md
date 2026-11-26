Issue-Source: trace-sweep-divergence
Issue-Trace: docdb-elastic-tag-resource-ops-missing-cluster-not-found
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: docdb-elastic-tag-resource-ops-missing-cluster-not-found
Issue-Shape-Id: com.amazonaws.docdbelastic#ChimeraDbLionfishServiceLambda
Issue-Key: docdb-elastic|ListTagsForResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Commit: 550897ef76cdf8c25c5b7a3311cfc4272dd0a559

# DocDB Elastic ListTagsForResource not-found refusal serves an x-amzn-errortype that fails the recorded header shape

> _Note: this issue was identified against the code base at commit `550897ef76cdf8c25c5b7a3311cfc4272dd0a559`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
The DocDB Elastic restJson1 error-response serializer stamps an x-amzn-errortype value on the ListTagsForResource not-found refusal that no real DocDB Elastic response carries. The status, error class, body and (now) the media type of this refusal are all correct; only the error-type header diverges. This bundle contains 1 cause; it must be closed before submitting.

Cause 1 of 1: ListTagsForResource against an ARN that no resource carries (fixture docdb-elastic-tag-resource-ops-missing-cluster-not-found, step task#2).

AWS: HTTP 404 with error code ResourceNotFoundException. Smithy (services/service-com-amazonaws-docdbelastic-chimeradblionfishservicelambda/smithy-model/com-amazonaws-docdbelastic-chimeradblionfishservicelambda.json): ChimeraDbLionfishServiceLambda, sdkId "DocDB Elastic", version 2022-11-28, protocols [restJson1], sigv4Name docdb-elastic; ListTagsForResource is GET /tags/{resourceArn}, @readonly, resourceArn @required @httpLabel @length 1-1011, errors ResourceNotFoundException (client, httpError 404), ValidationException (400), ThrottlingException (429), InternalServerException (500). For the x-amzn-errortype header itself the model says nothing (the service only @cors-exposes x-amzn-errortype, alongside x-amzn-requestid and x-amzn-trace-id), so this part is recorded behavior only: the corpus asserts the header's SHAPE, not a literal value, and the stable requirement that shape carries is that the value's LEADING token is the error code of the modeled error being served - here exactly the fixed text 'ResourceNotFoundException', taken from the error shape name, with nothing in it drawn from the request or from emulator state. The recorded shape is a QUALIFIED value: a bare error code with no qualifier does not satisfy it, and the qualifier's own text is environment-dependent and is NOT itself the requirement. Hypothesis: AWS serves the error code followed by a qualification of the modeled error shape (e.g. its shape id), so only the leading code is stable across runs; an Amazon-internal Coral namespace URL is not part of the public wire contract either.

Emulator: status 404, error class ResourceNotFoundException, body and content-type are all correct, but x-amzn-errortype is the bare 'ResourceNotFoundException', which does not satisfy the recorded header shape. Replay: $headers.x-amzn-errortype shape_mismatch (expected '<shaped-not-asserted>', got 'ResourceNotFoundException'); expected_status 404, actual_status 404; divergence_key service=docdb-elastic|ListTagsForResource|status=404->404|keys=$headers.x-amzn-errortype.

Why: x-amzn-errortype is the header SDKs, proxies and log pipelines read to classify an error BEFORE parsing the body. A client that validates the header's shape - or that splits it at the qualifier separator and requires the qualified form AWS serves - sees a value AWS never serves and falls into a generic-failure branch instead of the not-found branch, so retry policy and resource-provisioning logic keyed on ResourceNotFoundException takes a different path against the emulator than against AWS for a refusal that is otherwise byte-correct.

Fix: the DocDB Elastic crate's shared restJson1 error-response serializer - the single place x-amzn-errortype is written for this service's modeled errors - not the ListTagsForResource handler, whose status, error selection, body and media type are already correct.

Note on existing open issues (replay reports stub_path and owned_by EMPTY for task#2, so this cause is NOT already filed):
- 20261110T175506Z-docdb-elastic-tag-read-and-tag-delete-refusals.md is pinned to this same fixture/step (task#2) but its recorded keys are the content-type divergence (docdb-elastic|ListTagsForResource|4xx->4xx|$headers.content-type) and the Coral-URL-qualified header value; the emulator now answers application/json on this path and the bare code, so that issue's 'got' values no longer match what is served and replay no longer reports it as owning this cluster. Closing it can leave this header defect standing.
- 20261116T084534Z-docdb-elastic-tag-op-not-found-refusals-serve.md covers the same shared serializer fault on TagResource (task#1) and UntagResource (task#3) only; it does not carry ListTagsForResource.
- 20261104T053156Z-docdb-elastic-error-serializer-emits-a-coral-namespace.md is resolved and removed the Coral URL suffix, leaving the bare code - which still fails the same shape assertion. Note the oscillation: the fix must satisfy the recorded SHAPE (code + qualifier), not flip between two literals.
- 20261029T172712Z-docdb-elastic-responses-announce-content-type-application-json.md and 20261030T050126Z-docdb-elastic-pending-maintenance-action-responses-announce-content.md ask for the OPPOSITE media type (application/x-amz-json-1.1); the corpus expects application/json for these restJson1 responses, so following those issues would reintroduce a media-type divergence on this path.

Traces: docdb-elastic-tag-resource-ops-missing-cluster-not-found#task#2 (ListTagsForResource, Cause 1).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true; /__admin/health answered 200 and the :45197 data listener accepts). 16 steps total: 13 passed, 3 mismatched, 0 errored, 0 blocked; score 81.2%; divergence_count 3.
- ListTagsForResource, docdb-elastic-tag-resource-ops-missing-cluster-not-found#task#2: verdict 'mismatch'; expected_status 404, actual_status 404; detail '$headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'ResourceNotFoundException')'; divergence_key service=docdb-elastic|ListTagsForResource|status=404->404|keys=$headers.x-amzn-errortype; affected ['docdb-elastic-tag-resource-ops-missing-cluster-not-found#task#2'], affected_count 1; stub_path empty and owned_by EMPTY, so this cluster is not already filed (the fixture's other two clusters, task#1 and task#3, ARE owned by 20261116T084534Z-docdb-elastic-tag-op-not-found-refusals-serve.md and are excluded from this issue).
Requirements: HTTP 404 and error code ResourceNotFoundException (both already correct), and an x-amzn-errortype whose leading token is the fixed error code 'ResourceNotFoundException' in the qualified shape the recording asserts. Fixed text: the error code, taken from the modeled error shape name. Variable/environment-dependent: the qualifier that follows it, which is not itself asserted - the header is asserted by SHAPE, so no single recorded literal is the requirement, and the bare code alone does not satisfy it. No part of this header is drawn from the request or from emulator state.
Model basis: smithy_query on services/service-com-amazonaws-docdbelastic-chimeradblionfishservicelambda/smithy-model/com-amazonaws-docdbelastic-chimeradblionfishservicelambda.json - service_info (protocols [restJson1], sigv4Name docdb-elastic, arnNamespace docdb-elastic, version 2022-11-28, cors additionalExposedHeaders includes x-amzn-errortype) and operation ListTagsForResource (GET /tags/{resourceArn}, @readonly, resourceArn @required @httpLabel @length 1-1011, ResourceNotFoundException at httpError 404).

## Steps to Reproduce
1. Boot the emulator and target the DocDB Elastic (docdb-elastic) endpoint.
2. Send GET /tags/{resourceArn} (ListTagsForResource), sigv4-signed, with a well-formed docdb-elastic ARN that no resource carries.
3. Observe HTTP 404 with the ResourceNotFoundException error class and content-type application/json - all correct - then inspect the response headers: x-amzn-errortype is the bare 'ResourceNotFoundException' instead of the qualified value whose leading token is that error code.
4. Compare with the sibling POST /tags/{resourceArn} (TagResource) and DELETE /tags/{resourceArn}?tagKeys=<key> (UntagResource) refusals against the same unknown ARN, which show the identical header value - evidence the defect lives in the shared error serializer, not in a single handler.
5. Or replay fixture docdb-elastic-tag-resource-ops-missing-cluster-not-found, step task#2.
