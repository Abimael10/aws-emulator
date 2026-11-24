Issue-Source: trace-sweep-divergence
Issue-Trace: drs-replication-configuration-template-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-replication-configuration-template-lifecycle
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|CreateReplicationConfigurationTemplate|2xx->2xx|$.tags.resourceArn
Issue-Commit: 599e490a97a559fcc3995e99c1088cb4ddbb72a3

# drs CreateReplicationConfigurationTemplate 201 response drops the template's tags entries

> _Note: this issue was identified against the code base at commit `599e490a97a559fcc3995e99c1088cb4ddbb72a3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateReplicationConfigurationTemplate

## Description
One cause.

Cause 1 of 1: the 201 body of CreateReplicationConfigurationTemplate does not report the tag set the created template carries — the recorded response's `tags` entry is absent from the emulator's body.

AWS: CreateReplicationConfigurationTemplate is POST /CreateReplicationConfigurationTemplate answering HTTP 201 with the `com.amazonaws.drs#ReplicationConfigurationTemplate` structure (Smithy: operation CreateReplicationConfigurationTemplate, http {method: POST, uri: /CreateReplicationConfigurationTemplate, code: 201}, output shape ReplicationConfigurationTemplate). That output structure declares a `tags` member targeting TagsMap (a @sensitive map of TagKey len 0..256 -> TagValue len 0..256), documented as "A set of tags to be associated with the Replication Configuration Template resource", along`stagingAreaTags` (also TagsMap). The input declares the same optional `tags` member. So AWS reports, in the create's own 201, the tag set now associated with the template — every key/value the request asked to associate, under `$.tags`, keyed by the request's tag keys. The recorded step confirms it: the replayed response body carries `$.tags.<key>` (the fixture asserts the member's presence/shape at that path, with the key taken from the request's tag map — here the recorded key is `resourceArn`; that literal key is this recording's value, not part of the requirement). Status and headers match, so nothing about the error channel is at issue.

Emulator: the 201 is returned with the correct status (expected 201, got 201) but the body has no entry at `$.tags.<key>` — the replay reports `$.tags.resourceArn: missing (expected '<shaped-not-asserted>', got None)`. Either the `tags` map is serialized empty/absent, or the request's tag entries never reach it, so a client reading the created template back off the create sees no tags at all.

Why: a client that creates a tagged replication configuration template and reads `tags` off the create response (the usual way to confirm tagging without a second ListTagsForResource round trip, which is itself a 501 stub in this emulator — see the open issue "Implement drs TagResource: unimplemented scaffold stub" and "drs: 4 net-new behavioral root causes ...", neither of which covers this response-body gap) concludes the template is untagged. Tag-driven inventory, cost-allocation and tag-based lookup code then operates on a template it believes has no tags, and any assertion that the create echoed back what it asked to associate fails against the emulator while passing against AWS.

Fix: the CreateReplicationConfigurationTemplate response path — the handler's mapping of the request's `tags` into the stored record and the record's rendering to the wire structure (the record's to_wire, which is also what DescribeReplicationConfigurationTemplates and UpdateReplicationConfigurationTemplate report, so the same gap is visible on a later read of the same template). Hypothesis: the request's `tags` map is not carried into the stored record (or is stored but not serialized) because the member is @sensitive — the model's sensitivity governs logging only, never whether the member is placed on the wire. The template's tag set, including an empty map for a create that named none, must be reported under `$.tags` exactly as the request's keys and values give it.

Traces: drs-replication-configuration-template-lifecycle#task#7 (CreateReplicationConfigurationTemplate, verdict mismatch).

## Evidence
Replay of fixture drs-replication-configuration-template-lifecycle, step task#7, operation CreateReplicationConfigurationTemplate (service drs), verdict mismatch:

  expected_status: 201
  actual_status:   201
  detail: $.tags.resourceArn: missing (expected '<shaped-not-asserted>', got None)
  divergence_key: service=drs|CreateReplicationConfigurationTemplate|status=201->201|keys=$.tags.resourceArn
  affected: drs-replication-configuration-template-lifecycle#task#7 (1 step)

Expected: the 201 body carries the template's tag set under `$.tags`, with one entry per tag key the request associated (the recorded step's key is `resourceArn`; the key and value themselves come from the request, only their presence is a requirement).
Actual: the 201 body has no entry at that path — `tags` is empty or absent.

No status, error code or error message diverges on this step; the divergence is purely the missing response member. Model facts used: smithy_query(query="operation", operation="CreateReplicationConfigurationTemplate") and smithy_query(query="shape", shape="ReplicationConfigurationTemplate,TagsMap") on services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/smithy-model/com-amazonaws-drs-elasticdisasterrecoveryservice.json — output member `tags` -> TagsMap, @sensitive, optional; input member `tags` -> TagsMap, optional. Handler and rendering under inspection: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/create_replication_configuration_template.rs (tags mapping at the record construction, ~line 368) and src/state.rs ReplicationConfigurationTemplateRecord::to_wire (~line 113).

## Steps to Reproduce
1. Boot the emulator from this checkout and point a drs client at its data listener.
2. Call CreateReplicationConfigurationTemplate with a complete, valid request (stagingAreaSubnetId resolving to an existing subnet, replicationServersSecurityGroupsIDs, replicationServerInstanceType, associateDefaultSecurityGroup, useDedicatedReplicationServer, defaultLargeStagingDiskType, ebsEncryption, bandwidthThrottling, dataPlaneRouting, createPublicIP, stagingAreaTags, a 3-rule pitPolicy) AND a non-empty `tags` map, e.g. tags={"<anyKey>": "<anyValue>"}.
3. Observe the 201 response body: `tags` carries no entry for `<anyKey>` (it is empty or absent), while AWS returns the tag set the request associated.
4. Re-replay fixture drs-replication-configuration-template-lifecycle, step task#7, to confirm the fix.
