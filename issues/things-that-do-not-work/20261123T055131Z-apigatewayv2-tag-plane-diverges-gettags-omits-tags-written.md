Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-api-tag-key-value-boundaries
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-api-tag-key-value-boundaries apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found deleted-child-resource-not-found-messages
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|GetTags|2xx->2xx|$.tags.kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk
Issue-Key: apigatewayv2|GetTags|2xx->2xx|$.tags.key1
Issue-Key: apigatewayv2|GetTags|2xx->2xx|$.tags.key2
Issue-Key: apigatewayv2|GetTags|2xx->2xx|$.tags.sim2real-probe-tag
Issue-Key: apigatewayv2|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: 42b42c3bdabf251ec917d999bf0a8b3e884f4f35

# apigatewayv2 tag plane diverges: GetTags omits tags written after create and the UntagResource 204 is unframed (5 causes)

> _Note: this issue was identified against the code base at commit `42b42c3bdabf251ec917d999bf0a8b3e884f4f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTags

## Description
This bundle carries FIVE net-new causes on the API Gateway v2 tagging plane, all reported by the replay with service token `apigatewayv2`. Four of them are the same shared fault seen on four different resources/fixtures — a tag written to a resource AFTER the resource was created is never visible to GetTags — and the fifth is the response framing of the 204 that UntagResource answers. A fixer MUST close all five before submitting.

Cause 1 of 5 — GetTags omits a boundary-size tag that was written onto an existing API (fixture apigatewayv2-api-tag-key-value-boundaries, step task#3).
AWS: Smithy (apigatewayv2): TagResource = POST /v2/tags/{ResourceArn}, response code 201, input member Tags (jsonName `tags`, target map Tags); GetTags = GET /v2/tags/{ResourceArn}, code 200, output member Tags (jsonName `tags`). The Tags map's key is `__string` (no modeled length bound) and its value is `StringWithLengthBetween1And1600`, so a 128-character key paired with a 256-character value is inside every modeled constraint and AWS stores it; the recording shows GetTags then reporting that key with exactly the value that was written. Tagging in AWS is add-or-update on the resource's existing collection, so the key appears alongside the keys the resource already carried.
Emulator: the GetTags 200 body omits the key entirely — replay reports `$.tags.<128-char key>: missing (expected '<256-char value>', got None)`. The status is right (200) and the envelope is right; the map simply has no entry for the key that was just written.
Why: a client that tags a resource and then reads the tags back sees its write silently lost. Tag-driven code (cost allocation, ABAC-style conditions, inventory filters, any read-after-write confirmation) concludes the tag was never applied, while no error was ever returned to it. Boundary-size keys and values are also exactly where a client checks that the service did not truncate or drop its input, and here the whole entry disappears.
Fix: the tag-write path of this service and the state it shares with the tag-read path — the write must land in the same per-resource tag collection that GetTags projects, merged into (not replacing) the collection the resource already carries. In this crate TagResource is still a scaffolded handler that answers 501 and writes nothing, which is why no post-create write is ever observable; the auto-filed stub issue covers standing the operation up, but these GetTags reads are the recorded divergences and are what must be made to pass. Related open issues that do NOT cover this: `20261117T150941Z-apigatewayv2-gettags-stub.md` (GetTags itself now serves 200, so that stub report does not describe this), and `20261029T220031Z-apigatewayv2-responses-omit-defaulted-and-derived-members.md` (about defaulted/derived members of create/read projections, not about lost tag writes).
Traces: apigatewayv2-api-tag-key-value-boundaries#task#3. Handler cited: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/tag_resource.rs:141-159 (handle returns `Scaffolded::not_implemented("TagResource")`); the read side that must agree with it is services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/get_tags.rs.

Cause 2 of 5 — GetTags still reports the create-time value for an updated key and omits a newly added key (fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#9).
AWS: Smithy (apigatewayv2): TagResource POST /v2/tags/{ResourceArn} 201; GetTags GET /v2/tags/{ResourceArn} 200 with `tags`. AWS tagging is add-or-update: a TagResource naming a key the resource already carries REPLACES that key's value, and a key it does not carry is ADDED, leaving every other key untouched. The recording shows the read after that write carrying the updated value for the pre-existing key and carrying the newly added key.
Emulator: GetTags answers 200 but reports the PRE-UPDATE value for the key that was re-set, and has no entry at all for the key that was added — replay reports `$.tags.key1: value_mismatch (expected '<updated value>', got '<create-time value>')` and `$.tags.key2: missing (expected '<added value>', got None)`, the latter still absent after 30 polling attempts, so this is not eventual consistency.
Why: both halves of add-or-update are broken for a client. An update of an existing tag appears to succeed but the old value keeps being served, so a client that re-tags a resource to change an environment/owner marker keeps reading the stale marker; and an added tag never appears, so a client that polls for its own write waits forever (the 30 failed polls in the recording are exactly that client).
Fix: as Cause 1 — the tag-write path and the per-resource tag collection GetTags reads, with request entries merged over the stored collection so a named key takes the new value and an unnamed key keeps its own.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#9.

Cause 3 of 5 — the same read omits the added key on the second read of the same fixture (fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#11).
AWS: as Cause 2 — GetTags GET /v2/tags/{ResourceArn} 200 reports every key the resource carries, including one added by a later TagResource.
Emulator: GetTags answers 200 with the added key absent — `$.tags.key2: missing (expected '<added value>', got None)`.
Why: the gap is not transient and not confined to the first read after the write: a client that re-reads later still never sees the tag, so there is no retry or wait that recovers it.
Fix: as Cause 1.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#11.

Cause 4 of 5 — GetTags omits a tag written onto a resource in the deleted-child fixture (fixture deleted-child-resource-not-found-messages, step task#32).
AWS: as Cause 1 — GetTags GET /v2/tags/{ResourceArn} 200 reports the probe tag that TagResource wrote onto the resource.
Emulator: GetTags answers 200 with that key absent — `$.tags.<probe key>: missing (expected '<probe value>', got None)`.
Why: the loss is not specific to one resource kind or one fixture; any client that tags any v2 resource after creation reads back a collection missing its write.
Fix: as Cause 1.
Traces: deleted-child-resource-not-found-messages#task#32.

Cause 5 of 5 — the UntagResource 204 response carries no Content-Length header (fixture apigatewayv2-api-tag-key-value-boundaries, step task#7).
AWS: Smithy (apigatewayv2): UntagResource = DELETE /v2/tags/{ResourceArn}, response code 204, output Unit, with TagKeys bound to the `tagKeys` query string. AWS frames that empty-bodied 204 with an explicit `Content-Length: 0` header; the recording asserts the header is present on the response (its shaped value is `0`, the only length an empty body can have). The status itself is correct in the emulator.
Emulator: the 204 is answered with the `content-length` header absent entirely — replay reports `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`. Status 204 matches; only the framing header is dropped.
Why: a client or proxy that reads message framing from Content-Length rather than inferring it from the status sees an unframed response. Strict HTTP clients, recording proxies and SDK response-length checks treat a response with neither Content-Length nor a terminating framing signal differently from AWS's, which can leave a connection waiting on a body that will never come instead of completing the tag removal.
Fix: this service's response serialization for empty-bodied success responses — the 204 writer must emit `Content-Length: 0` for operations whose output is Unit, UntagResource included. Related open issues that do NOT cover this operation: `20261117T231134Z-apigatewayv2-204-delete-responses-omit-the-content-length.md` and `20261106T033429Z-apigatewayv2-204-deletes-omit-the-content-length-0.md` both own the Delete* operations (DeleteApi, DeleteStage, DeleteRoute, DeleteApiMapping), and the replay's ownership marking does not place UntagResource under either, so this cause is net-new and must be closed here.
Traces: apigatewayv2-api-tag-key-value-boundaries#task#7 (cluster also covers apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#10, #task#12 and apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#13).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (116 fixtures, 1057 steps, score 86.2%).

Cause 1 — fixture apigatewayv2-api-tag-key-value-boundaries step task#3, GetTags, expected 200 / actual 200:
  $.tags.<128-char key 'kkkk...'>: missing (expected '<256-char value 'vvvv...'>', got None)

Cause 2 — fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags step task#9, GetTags, expected 200 / actual 200:
  $.tags.key1: value_mismatch (expected 'sim2real-value1updated', got 'sim2real-value1')
  $.tags.key2: missing (expected 'sim2real-value2', got None)
  $.tags.key2: poll: still absent after 30 attempt(s) (expected 'sim2real-value2', got None)
  (cluster also covers apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#12)

Cause 3 — fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags step task#11, GetTags, expected 200 / actual 200:
  $.tags.key2: missing (expected 'sim2real-value2', got None)

Cause 4 — fixture deleted-child-resource-not-found-messages step task#32, GetTags, expected 200 / actual 200:
  $.tags.sim2real-probe-tag: missing (expected 'live', got None)

Cause 5 — fixture apigatewayv2-api-tag-key-value-boundaries step task#7, UntagResource, expected 204 / actual 204:
  $headers.content-length: missing (expected '<shaped-not-asserted>', got None)
  (cluster also covers apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#10 and #task#12, and apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#13)

Model evidence: smithy_query(operation="TagResource,UntagResource,GetTags", model_path="services/service-com-amazonaws-apigatewayv2-apigatewayv2/smithy-model/com-amazonaws-apigatewayv2-apigatewayv2.json") — TagResource POST /v2/tags/{ResourceArn} code 201; GetTags GET /v2/tags/{ResourceArn} code 200, output Tags (jsonName 'tags'); UntagResource DELETE /v2/tags/{ResourceArn} code 204, output Unit. smithy_query(shape="Tags") — map<__string, StringWithLengthBetween1And1600>.

Source evidence for the write side never landing: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/ops/tag_resource.rs still carries the '//! SCAFFOLDED, NOT IMPLEMENTED' marker and its handle() body ends with Err(crate::scaffolded::Scaffolded::not_implemented("TagResource")) at line 158, while src/ops/get_tags.rs and src/ops/untag_resource.rs are implemented.

## Steps to Reproduce
1. git clone the repo and build the emulator from this checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: replay fixture apigatewayv2-api-tag-key-value-boundaries and inspect step task#3 — the GetTags 200 body's `tags` map has no entry for the 128-character key written by the preceding TagResource step.
4. Cause 2/3: replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags and inspect steps task#9 and task#11 — `tags.key1` still holds its create-time value instead of the value the later TagResource set, and `tags.key2` is absent on both reads.
5. Cause 4: replay fixture deleted-child-resource-not-found-messages step task#32 — the probe tag written on the resource is absent from the GetTags 200 body.
6. Cause 5: replay fixture apigatewayv2-api-tag-key-value-boundaries step task#7 — the UntagResource response is 204 but carries no `content-length` response header.
7. Equivalent by hand: CreateApi with tags, then POST /v2/tags/<api arn> adding a new key and re-setting an existing one, then GET /v2/tags/<api arn> (the added key is missing and the re-set key is stale), then DELETE /v2/tags/<api arn>?tagKeys=<key> and inspect the response headers (no Content-Length).
