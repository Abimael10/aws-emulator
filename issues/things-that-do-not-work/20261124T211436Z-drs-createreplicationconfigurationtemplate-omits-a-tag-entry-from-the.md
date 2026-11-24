Issue-Source: trace-sweep-divergence
Issue-Trace: drs-replication-configuration-template-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-replication-configuration-template-lifecycle
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|CreateReplicationConfigurationTemplate|2xx->2xx|$.tags.resourceArn
Issue-Commit: c6dd973eb6cd65a31ae8eee55c5b01fc505934bd

# drs CreateReplicationConfigurationTemplate omits a tag entry from the created template's tags map in its 201 body

> _Note: this issue was identified against the code base at commit `c6dd973eb6cd65a31ae8eee55c5b01fc505934bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationConfigurationTemplate

## Description
One net-new wire divergence in the drs (Elastic Disaster Recovery) emulator, unowned by any open issue and not blocked by any stub (the replay reports an empty owned_by and an empty stub_path for it).

Cause 1 of 1: the 201 body of CreateReplicationConfigurationTemplate reports a tags map that does not carry the tag entry the created template carries — the entry keyed 'resourceArn' is absent from $.tags. (fixture drs-replication-configuration-template-lifecycle, step task#7)

AWS: Smithy (com.amazonaws.drs) models CreateReplicationConfigurationTemplate POST /CreateReplicationConfigurationTemplate with http code 201 and output shape ReplicationConfigurationTemplate, which declares tags: TagsMap (@sensitive map, optional) documented as "A set of tags to be associated with the Replication Configuration Template resource." The input shape declares the same optional tags: TagsMap. The model states no default for that member and no service-side tag of its own, so what a create must REPORT in tags is recorded behavior only: the created template's complete tag set, i.e. every key/value pair that template carries once it exists, reported in the create's own 201 body so that a following Describe/ListTagsForResource cannot disagree with it. The recorded 201 carries a tags entry under the key 'resourceArn'; the recording asserts that entry's PRESENCE and the SHAPE of its value only ('<shaped-not-asserted>'), because the value is an ARN that differs per environment and per run. Fixed part of the requirement: the key is present in $.tags of the 201. Variable part: the value, an ARN built from the partition/region/account the request was signed for and the id the service just minted — never the literal ARN of any one recording.

Emulator: the step gets HTTP 201 as expected, and every other member of the body matches, but $.tags.resourceArn is missing (got None) — the tags map the 201 reports does not contain that entry. The create handler builds the stored record's tag set from the request's tags member alone (crate::ops::create_replication_configuration_template, tags: input.tags.unwrap_or_default()) and reports it through ReplicationConfigurationTemplateRecord::to_wire (tags: Some(self.tags.clone())), so the only pairs that can ever appear in the 201 are the ones the request body named, verbatim. Hypothesis: the entry the recording asserts is one the emulator never puts into the template's tag set — either a pair the request supplied that the create path does not carry into the stored record, or a pair the service itself records on the resource at create time (an ARN-valued entry it can only build after the id is minted) — so the created template's reported tag set is a strict subset of the one it actually carries.

Why: a client reads a newly created resource's tags off the create response — that is the point of echoing tags in the output shape. With an entry missing, tag-driven code goes wrong in ways that do not look like a tagging bug: a caller that keys its own bookkeeping off the created template's tags (cost allocation, ownership, cleanup selection) never sees the entry, an idempotent provisioner that compares desired tags against the create response sees a drift that is not there and re-tags on every run, and a cleanup pass that selects resources by that tag skips the template and leaks it. Because the tagging route reads the SAME stored map (crate::state::tags::resource_tags), the omission is not confined to the create body: ListTagsForResource for that template reports the same incomplete set.

Fix: the create path's tag handling for a replication configuration template — the tag set stored under the minted id in crate::state::ReplicationConfigurationTemplateRecord::tags must be the complete set the created template carries, and ReplicationConfigurationTemplateRecord::to_wire must report it, so the 201, a later Describe and a later ListTagsForResource all agree. No other open drs issue covers this step: 20261119T235246Z-drs-4-net-new-causes-tagging-route-answers.md covers the tagging route's statuses/refusals and a launch-configuration-template read-back, and 20261117T220225Z-drs-recovery-plan-lifecycle-is-not-faithful-status.md covers the recovery-plan lifecycle; neither touches CreateReplicationConfigurationTemplate's tags map.

Traces: drs-replication-configuration-template-lifecycle#task#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 68 steps, 42 passed, 26 mismatched, score 61.8%).

Cause 1 — fixture drs-replication-configuration-template-lifecycle, step task#7, CreateReplicationConfigurationTemplate: expected HTTP 201, actual HTTP 201 (status agrees); body divergence `$.tags.resourceArn: missing (expected '<shaped-not-asserted>', got None)`. Divergence key: service=drs|CreateReplicationConfigurationTemplate|status=201->201|keys=$.tags.resourceArn. Cluster: 1 affected step (drs-replication-configuration-template-lifecycle#task#7); owned_by empty, stub_path empty — not covered by any open issue and not blocked by an unimplemented operation.

Model: smithy_query(query="operation", operation="CreateReplicationConfigurationTemplate", model_path="services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/smithy-model/com-amazonaws-drs-elasticdisasterrecoveryservice.json") -> input member tags {target: TagsMap, sensitive: true, type: map} (optional), output member tags {target: TagsMap, sensitive: true, type: map}, output_shape ReplicationConfigurationTemplate, http {method: POST, uri: /CreateReplicationConfigurationTemplate, code: 201}. smithy_query(query="doc", shape="ReplicationConfigurationTemplate", member="tags") -> "A set of tags to be associated with the Replication Configuration Template resource." The model declares no default for tags and no service-assigned tag.

Code: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_replication_configuration_template.rs line 368 (`tags: input.tags.unwrap_or_default()` — the stored tag set is exactly what the request body named, and nothing else can ever enter it) and lines 380-386 (the 201 reports the stored record via `to_wire`); src/state.rs lines 76-78 (ReplicationConfigurationTemplateRecord::tags) and line 130 (`tags: Some(self.tags.clone())` in `to_wire`, the one place a stored template becomes a wire template); src/state/tags.rs lines 171-175 (the tagging route reads that SAME map, so the create body and ListTagsForResource for this template report the identical incomplete set).

## Steps to Reproduce
1. git clone the repo and build/boot the drs emulator from this checkout (services/service-com-amazonaws-drs-elasticdisasterrecoveryservice).
2. Replay trace set simparity-wire-aws-v1.
3. Cause 1: fixture drs-replication-configuration-template-lifecycle, step task#7 — POST /CreateReplicationConfigurationTemplate for the lifecycle's template. Expect HTTP 201 whose $.tags map contains the entry keyed 'resourceArn' (value an ARN built from the signed partition/region/account and the freshly minted template id — assert its shape, not a literal value from any one run). Observe HTTP 201 whose $.tags map has no 'resourceArn' entry at all.
4. Confirm the knock-on: GET /tags/{resourceArn} (ListTagsForResource) for the template's ARN reports the same incomplete tag set, since both read crate::state::ReplicationConfigurationTemplateRecord::tags.
5. Re-replay drs-replication-configuration-template-lifecycle#task#7 to confirm a fix: the step must pass on its own.
