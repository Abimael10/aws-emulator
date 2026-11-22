Issue-Source: trace-sweep-divergence
Issue-Trace: redshiftserverless-namespace-lifecycle
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: redshiftserverless-namespace-lifecycle
Issue-Shape-Id: com.amazonaws.redshiftserverless#RedshiftServerless
Issue-Key: redshift-serverless|ListTagsForResource|2xx->2xx|$.tags[*]
Issue-Commit: a2d6c7a74c3b04e1146167d89a2a7f728862e388

# redshift-serverless ListTagsForResource answers an empty tag list for a tagged namespace

> _Note: this issue was identified against the code base at commit `a2d6c7a74c3b04e1146167d89a2a7f728862e388`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Cause 1 of 1: ListTagsForResource loses the tag set of the namespace the ARN resolves to, reporting an empty TagList for a resource that carries tags.

AWS: ListTagsForResource "Lists the tags assigned to a resource" and returns `tags` (Smithy: ListTagsForResource output member `tags` -> TagList, @length min 0 max 200; input `resourceArn` -> AmazonResourceName @length 1..1011 @required; errors InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400). The tags a resource carries are STATE: CreateNamespace models `tags` (TagList, @length 0..200, "A list of tag instances") and TooManyTagsException, so the key/value pairs supplied when the resource is created — or later attached to it — are owned by the resource and every subsequent ListTagsForResource on that resource's ARN must report exactly that set, one `{key, value}` member per tag, until a call changes it. A resource whose ARN resolves is answered with its stored set (HTTP 200); only an ARN that names no resource of this account in this Region is the 404 ResourceNotFoundException. The tag keys and values themselves are request values, never emulator-derived.

Emulator: the call is answered HTTP 200 — so the ARN DID resolve to a stored resource — but the returned `tags` list is empty: `$.tags[0]` is missing where the recorded response carries the tag that was attached to that namespace earlier in the same fixture (recorded example: `{"key": "sim2real", "value": "namespace-lifecycle"}` — the key and value are fixture request values, the requirement is that whatever pair was attached comes back, not these literals). No tag of the resource is reported at all, so the whole tag set is dropped rather than one member being wrong.

Why: a client that tags a Redshift Serverless namespace and then reads its tags back sees an untagged resource. Tag-driven logic breaks silently and non-retryably on a 200: cost-allocation/ownership checks conclude the resource is untagged, idempotent "tag if missing" loops re-tag forever, and tooling that enforces mandatory tags (or deletes untagged resources) acts on a resource that is in fact tagged. Because the status is 200 and the shape is valid, no client can distinguish this from a genuinely untagged resource.

Fix: the redshift-serverless tag state and the handlers that write it — `tags_for_resource_arn` in the service state and the record-minting paths that set a resource's `tags` field — not the ListTagsForResource serializer, which faithfully projects whatever the record holds. Hypothesis: the namespace record the ARN resolves to at this point in the lifecycle is one minted WITHOUT carrying over the tag set (a namespace minted by the restore path is constructed with an empty tag vector, while the resource identity the client holds is expected to keep its tags), or the tag set written by the create/tagging call is not the one the record that ARN resolves to holds. Either way the stored set is empty where the resource has tags, and the projection must report the resource's actual tag set.

Not covered by an existing issue: the open issue `20261114T150622Z-redshift-serverless-tagresource-stub.md` (TagResource is a 501 scaffold) is a different defect — this step is a 200 answer from an IMPLEMENTED ListTagsForResource that resolved the resource and then reported no tags, and no step of this fixture was blocked by a stub.

Traces: redshiftserverless-namespace-lifecycle#task#6 (ListTagsForResource). Code citations: services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/ops/list_tags_for_resource.rs:156-175 (projects `state.tags_for_resource_arn(...)`), services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/state.rs:591-608 (`tags_for_resource_arn`), services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/ops/restore_from_snapshot.rs:470-478 (minted namespace record set to `tags: Vec::new()`), services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/ops/create_namespace.rs:248-255 (create stores `input.tags`).

## Evidence
Replay of fixture `redshiftserverless-namespace-lifecycle`, step `task#6`, operation ListTagsForResource (service redshift-serverless):
  expected_status 200, actual_status 200 — verdict: mismatch
  detail: `$.tags[0]: missing (expected {'key': 'sim2real', 'value': 'namespace-lifecycle'}, got None)`

Expected (AWS): HTTP 200 with `tags` containing one member per tag assigned to the resource — `{"key": <key from the tagging request>, "value": <value from the tagging request>}`.
Actual (emulator): HTTP 200 with `tags` present but EMPTY — no member at index 0 — for an ARN the emulator itself resolved to a stored namespace (an unresolvable ARN would have been the modelled ResourceNotFoundException/404 instead, which is not what happened).
Smithy model consulted: services/service-com-amazonaws-redshiftserverless-redshiftserverless/smithy-model/com-amazonaws-redshiftserverless-redshiftserverless.json — ListTagsForResource (output `tags`: TagList, length 0..200; errors InternalServerException/ResourceNotFoundException/ThrottlingException/ValidationException) and CreateNamespace (input `tags`: TagList, length 0..200; error TooManyTagsException).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire fixture `redshiftserverless-namespace-lifecycle` (trace set simparity-wire-aws-v1).
2. Drive the namespace lifecycle the fixture drives: create a redshift-serverless namespace carrying a tag in the create request (`tags: [{key: <k>, value: <v>}]`) and continue through the fixture's lifecycle steps.
3. At step `task#6`, call ListTagsForResource with the namespace's `namespaceArn` as `resourceArn`.
4. Observe HTTP 200 with an empty `tags` list, instead of HTTP 200 with one `{key, value}` member per tag assigned to that namespace.
