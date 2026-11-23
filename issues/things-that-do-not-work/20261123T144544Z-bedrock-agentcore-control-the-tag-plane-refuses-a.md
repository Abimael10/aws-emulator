Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-memory-tag-list-untag-roundtrip
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-memory-tag-list-untag-roundtrip
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|ListTagsForResource|2xx->4xx|$.tags
Issue-Key: bedrock-agentcore-control|ListTagsForResource|2xx->4xx|status
Issue-Key: bedrock-agentcore-control|TagResource|2xx->4xx|status
Issue-Key: bedrock-agentcore-control|UntagResource|2xx->4xx|status
Issue-Commit: 9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9

# bedrock-agentcore-control: the tag plane refuses a Memory ARN as an untaggable resource type, so all three tagging operations are unusable for memories (3 causes)

> _Note: this issue was identified against the code base at commit `9e4314a6b919d5458fdf4a3f91d7ac4b6bc489f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Three net-new wire causes, all reached through a Memory resource ARN (`arn:aws:bedrock-agentcore:<region>:<account>:memory/<memoryId>`), and all three answered HTTP 400 by the SAME shared ARN classification: this service's tag plane does not list `memory` as a taggable resource type, so a memory ARN resolves to `TagTarget::OutOfScope` and every tag call against a memory is refused as a request defect instead of being served. They are listed as three causes because each of the three operations has its own recorded, differing correct answer (a 200 body, and two 204 no-content answers) and each must be re-replayed to be closed.

Not covered by the open issue 20261123T121913Z-bedrock-agentcore-control-the-tag-plane-does-not.md — that one is about the NESTED runtime-endpoint ARN (`runtime/<id>/runtime-endpoint/<name>`) being split at the first `/` and answered 404 NotFoundException; a memory ARN parses cleanly into type + id and is refused 400 ValidationException by a different arm of the same match. Nor by 20261029T212239Z-bedrock-agentcore-control-listtagsforresource-stub.md (ListTagsForResource is implemented and answers — it answers wrongly), nor by any other open bedrock-agentcore-control issue.

Cause 1 of 3: ListTagsForResource on an existing memory's ARN is refused HTTP 400 instead of reporting the memory's tag set.
AWS: ListTagsForResource takes `resourceArn` of shape `TaggableResourcesArn` (Smithy: ListTagsForResource.resourceArn -> TaggableResourcesArn, @required, @httpLabel, @length 20..1011, @pattern `^arn:(?:[^:]+)?:bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:([a-z-]+/[^/]+)(?:/[a-z-]+/[^/]+)*$`), returns `{tags: TagsMap}` with http code 200, and declares ValidationException 400 / ResourceNotFoundException 404 / AccessDeniedException 403. A memory ARN (`memory/<memoryId>`) satisfies that pattern, and CreateMemory itself persists a tag set (Smithy: CreateMemory.tags -> TagsMap @length 0..50). The operation's doc note ("This feature is currently available only for AgentCore Runtime, Browser, Browser Profile, Code Interpreter tool, and Gateway") does NOT list Memory, but the recording contradicts that note: recorded behavior shows AWS answering HTTP 200 with the memory's current tag map — the keys and values the create request carried, then the keys TagResource added, then the set left after UntagResource removed a key.
Emulator: HTTP 400 `ValidationException`, with no `tags` member in the body. The refusal is the tag plane's shared member refusal for an untaggable type: fixed text `tagging is not supported for this resource type` reported against the member name `resourceArn` (the member name is the variable part, taken from the request's refused member; there is no other variable part). Cause: `TAGGABLE_ARN_RESOURCES` omits the memory ARN resource type, so `tag_target()` falls through to the `AGENTCORE_RESOURCE_TYPES` arm and returns `TagTarget::OutOfScope`, which this handler turns into `tagging_unsupported_type()`. `resource_tags()` likewise has no memory arm, so even a resolved memory key would report nothing.
Why: a client cannot read the tags of a memory at all. Cost-allocation, inventory and discovery tooling that lists tags per resource ARN gets a hard validation failure for every memory in the account, and a client branching on ValidationException concludes its own request is malformed when the ARN is exactly the one CreateMemory/GetMemory handed it.
Fix: the tag plane's shared ARN classification and tag-set resolution in the service state (the `TAGGABLE_ARN_RESOURCES` set and the `tag_target` / `resource_tags` / `resource_tags_mut` match arms the three tagging handlers branch on) must recognise the memory resource type and reach the memory record's own `tags` map.
Traces: bedrock-agentcore-control-memory-tag-list-untag-roundtrip#task#2 (also task#4 and task#6 — the same read after the tag write and after the untag).

Cause 2 of 3: TagResource on an existing memory's ARN is refused HTTP 400 instead of writing the tags and answering 204 No Content.
AWS: Smithy: TagResource POST /tags/{resourceArn}, success http code 204 with an empty output shape; inputs `resourceArn` (TaggableResourcesArn, @required, @httpLabel) and `tags` (TagsMap, @required, @length 0..50); errors ValidationException 400 / ResourceNotFoundException 404 / ServiceQuotaExceededException 402 / AccessDeniedException 403. Documented semantics: "Associates the specified tags to a resource with the specified resourceArn. If existing tags on a resource are not specified in the request parameters, they are not changed." Recorded behavior: against a memory ARN the call answers HTTP 204 with no body, and the added keys are then visible through ListTagsForResource merged with the keys the memory already carried.
Emulator: HTTP 400 `ValidationException` with the same fixed text `tagging is not supported for this resource type` against member `resourceArn`; nothing is written. Same root cause as cause 1 — TagResource branches on the same `TagTarget::OutOfScope` arm.
Why: a memory's tags can never be changed after creation. A client that tags resources post-create (the normal pattern for ABAC and cost allocation) is refused, and since the refusal is a validation error rather than an unsupported-operation signal, retrying or correcting the request can never succeed.
Fix: same component as cause 1; once the memory type is taggable, TagResource's merge-and-write path must reach the memory record's `tags` map and answer the modelled 204.
Traces: bedrock-agentcore-control-memory-tag-list-untag-roundtrip#task#3.

Cause 3 of 3: UntagResource on an existing memory's ARN is refused HTTP 400 instead of removing the named keys and answering 204 No Content.
AWS: Smithy: UntagResource DELETE /tags/{resourceArn}, @idempotent, success http code 204 with an empty output shape; inputs `resourceArn` (TaggableResourcesArn, @required, @httpLabel) and `tagKeys` (TagKeyList, @required, @httpQuery "tagKeys", @length 0..200); errors ValidationException 400 / ResourceNotFoundException 404 / AccessDeniedException 403. Recorded behavior: against a memory ARN the call answers HTTP 204 with no body, and the named keys are absent from the following ListTagsForResource while the unnamed keys remain.
Emulator: HTTP 400 `ValidationException`, fixed text `tagging is not supported for this resource type` against member `resourceArn`; no key is removed. Same shared classification as causes 1 and 2.
Why: tag keys written on a memory (including at create time) can never be removed, so a client cannot undo a tag or clean up before deleting; the tag surface is write-once-at-create and read-never.
Fix: same component as causes 1 and 2; UntagResource's key-removal path must reach the memory record's `tags` map and answer the modelled 204.
Traces: bedrock-agentcore-control-memory-tag-list-untag-roundtrip#task#5.

## Evidence
Replayed fixture `bedrock-agentcore-control-memory-tag-list-untag-roundtrip` (trace set simparity-wire-aws-v1).

Cause 1 — step task#2 (also task#4, task#6), ListTagsForResource:
  expected: HTTP 200 with a `tags` map holding the memory's current tag set (create-time keys, then the keys TagResource added, then the set left after UntagResource)
  actual:   HTTP 400, no `tags` member in the body
  replay detail: "status: expected 200, got 400 / $.tags: missing (expected {...}, got None)"
  emulator error served: ValidationException with fieldList entry name=`resourceArn`, message=`tagging is not supported for this resource type` (fixed text; `resourceArn` is the refused member name from the request)

Cause 2 — step task#3, TagResource:
  expected: HTTP 204, empty body
  actual:   HTTP 400
  replay detail: "status: expected 204, got 400"

Cause 3 — step task#5, UntagResource:
  expected: HTTP 204, empty body
  actual:   HTTP 400
  replay detail: "status: expected 204, got 400"

Source grounding (one shared classification behind all three):
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/tags.rs lines 58-66: `TAGGABLE_ARN_RESOURCES` lists only runtime, gateway, browser (+system), browser-profile, code-interpreter (+system) — no memory type.
- same file lines 157-205: `tag_target()` splits the ARN resource part into type + id, finds the memory type absent from `TAGGABLE_ARN_RESOURCES` but present in `AGENTCORE_RESOURCE_TYPES`, and returns `TagTarget::OutOfScope`.
- same file lines 124-129: `tagging_unsupported_type()` builds the ValidationException from `field_validation_failed(RESOURCE_ARN_MEMBER, "tagging is not supported for this resource type")`.
- same file lines 209-238: `resource_tags()` / `resource_tags_mut()` have no memory arm.
- services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/list_tags_for_resource.rs lines 170-188 serve that refusal; ops/tag_resource.rs and ops/untag_resource.rs branch on the identical classification.

Smithy consulted: TagResource / UntagResource / ListTagsForResource signatures, errors and http bindings (204/204/200), `TaggableResourcesArn` @pattern and @length, `TagsMap` @length 0..50, CreateMemory.tags, and the TagResource doc note (which omits Memory and is contradicted by the recording).

## Steps to Reproduce
1. CreateMemory(name=<name>, eventExpiryDuration=<days>, tags={<some keys>}) and wait until the memory is ACTIVE; take the memory id / ARN (`arn:aws:bedrock-agentcore:<region>:<account>:memory/<memoryId>`).
2. ListTagsForResource(resourceArn=<memoryArn>) — AWS answers HTTP 200 with the create-time tag map; the emulator answers HTTP 400 ValidationException `resourceArn: tagging is not supported for this resource type`.
3. TagResource(resourceArn=<memoryArn>, tags={<new key>: <value>}) — AWS answers HTTP 204 with no body; the emulator answers the same HTTP 400 and writes nothing.
4. ListTagsForResource(resourceArn=<memoryArn>) again — AWS reports the merged set (create-time keys plus the new key); the emulator still answers 400.
5. UntagResource(resourceArn=<memoryArn>, tagKeys=[<one of the keys>]) — AWS answers HTTP 204 with no body; the emulator answers the same HTTP 400 and removes nothing.
6. ListTagsForResource(resourceArn=<memoryArn>) a third time — AWS reports the set minus the removed key; the emulator still answers 400.
7. Replay fixture `bedrock-agentcore-control-memory-tag-list-untag-roundtrip` steps task#2, task#3, task#4, task#5, task#6.
