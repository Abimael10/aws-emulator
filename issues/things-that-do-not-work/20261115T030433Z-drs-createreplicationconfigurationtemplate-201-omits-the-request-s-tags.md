Issue-Source: trace-sweep-divergence
Issue-Trace: drs-replication-configuration-template-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-replication-configuration-template-lifecycle
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|CreateReplicationConfigurationTemplate|2xx->2xx|$.tags.resourceArn
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# drs CreateReplicationConfigurationTemplate 201 omits the request's tags entries from the created template

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationConfigurationTemplate

## Description
One net-new wire root cause in the Elastic Disaster Recovery (drs) emulator.

Cause 1 of 1: the 201 answer to CreateReplicationConfigurationTemplate does not report the tag set the created template carries — the `tags` member of the returned ReplicationConfigurationTemplate is missing an entry the recorded response carries (the tag key `resourceArn`).

AWS: CreateReplicationConfigurationTemplate is POST /CreateReplicationConfigurationTemplate with success status 201 and output shape ReplicationConfigurationTemplate (Smithy: operation CreateReplicationConfigurationTemplate, http {method: POST, uri: /CreateReplicationConfigurationTemplate, code: 201}). Both input and output declare `tags` targeting TagsMap (a map<String,String>, @sensitive), documented on the output shape as "A set of tags to be associated with the Replication Configuration Template resource" (Smithy: ReplicationConfigurationTemplate$tags doc). The created template therefore reports the tag set it was created with: every key/value entry the request's `tags` map carried appears, verbatim, in the `tags` map of the 201 body (and in every later DescribeReplicationConfigurationTemplates / UpdateReplicationConfigurationTemplate read of the same template). The recorded 201 for this step contains a `tags` map holding the key `resourceArn`; the key is the tag name the request supplied, and its value is request data, not fixed text — the requirement is that the entry is present and unchanged, not that it holds any particular literal.

Emulator: the emulator answers 201 (the status matches) but the body's `tags` map has no `resourceArn` entry at all — the replay reports `$.tags.resourceArn: missing (expected '<shaped-not-asserted>', got None)`, i.e. the key is absent from the emulated response rather than present with a differing value. The tag set the create was asked to attach is not reported back. Hypothesis: the request's `tags` map is not reaching the stored record (the handler stores `input.tags.unwrap_or_default()`, so a `tags` member that never deserializes is indistinguishable from one that was omitted, and the record then renders an empty map), rather than the record→wire mapping dropping the member — `ReplicationConfigurationTemplateRecord::to_wire` does emit `tags: Some(..)`.

Why: a client that creates a tagged replication configuration template reads the created resource's tags straight off the create response (that is the only read AWS gives it without a second call) and gets an empty/short tag set. Tag-driven logic — cost allocation, ownership/inventory lookups, policy conditions keyed on a tag, and any "create then assert my tags are attached" step — concludes the tags were not applied and either re-tags or fails, and the same loss propagates to Describe/Update reads of the template, so the tagging contract of the whole resource looks broken even though the create succeeded.

Fix: the CreateReplicationConfigurationTemplate handler's tag handling and the `tags` member's input deserialization — services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_replication_configuration_template.rs (the `record` construction, `tags: input.tags.unwrap_or_default()`) together with the stored record's wire rendering in src/state.rs (`ReplicationConfigurationTemplateRecord::to_wire`) — so that every entry of the request's `tags` map is persisted on the record and reported in the 201 and in all later reads of that template.

Note on related open issues: the open drs issues cover the account-initialization gate, the stagingAreaSubnetId existence check, out-of-enum rejection, the three tag operations' 501 stubs (20261109T163249Z-drs-4-net-new-behavioral-root-causes-the, 20261110T042638Z-drs-tagresource-stub) and request-body validation error shapes (20261110T042638Z-drs-request-body-validation-refusals-use-the-wrong). None of them covers the create response failing to echo the template's own `tags` map; this is a distinct defect in CreateReplicationConfigurationTemplate's success path.

Traces: drs-replication-configuration-template-lifecycle#task#7.

## Evidence
Wire replay, fixture drs-replication-configuration-template-lifecycle, step task#7, operation CreateReplicationConfigurationTemplate (service drs):
  expected status 201, actual status 201 (status matches)
  body diff: $.tags.resourceArn: missing (expected '<shaped-not-asserted>', got None)
  verdict: mismatch; divergence_key service=drs|CreateReplicationConfigurationTemplate|status=201->201|keys=$.tags.resourceArn
  affected: drs-replication-configuration-template-lifecycle#task#7 (cluster of 1)

Expected (AWS): the 201 body is the created ReplicationConfigurationTemplate with a `tags` map containing the entries the request supplied — in this recording an entry under the key `resourceArn`. The key is the tag name from the request and the value is request data; only the presence and fidelity of the entry is the requirement.
Actual (emulator): the 201 body carries no `tags` entry for that key.

Model evidence: smithy_query(operation="CreateReplicationConfigurationTemplate") — input member `tags` -> TagsMap (map, @sensitive), output shape ReplicationConfigurationTemplate member `tags` -> TagsMap (map, @sensitive), http code 201. smithy_query(doc, shape=ReplicationConfigurationTemplate, member=tags) — "A set of tags to be associated with the Replication Configuration Template resource."
Code: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_replication_configuration_template.rs:368 (`tags: input.tags.unwrap_or_default()`); src/state.rs:113 (`tags: Some(self.tags.clone())`).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the bound wire corpus (replay_wire_corpus), or point a drs client at the emulator directly.
2. Call CreateReplicationConfigurationTemplate with a complete, valid body (stagingAreaSubnetId naming a subnet the emulator's EC2 state holds, associateDefaultSecurityGroup, replicationServersSecurityGroupsIDs, replicationServerInstanceType, useDedicatedReplicationServer, defaultLargeStagingDiskType, ebsEncryption, bandwidthThrottling, dataPlaneRouting, createPublicIP, stagingAreaTags, a 3-rule pitPolicy) AND a non-empty `tags` map, e.g. tags={"resourceArn": "<any value>"}.
3. Read the 201 response body: its `tags` map does not contain the key that was sent (the member comes back empty/without that entry) instead of echoing every supplied entry.
4. Re-replay drs-replication-configuration-template-lifecycle, step task#7, to confirm the fix.
