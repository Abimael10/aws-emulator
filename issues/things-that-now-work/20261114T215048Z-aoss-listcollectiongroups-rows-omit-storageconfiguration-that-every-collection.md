Issue-Source: trace-sweep-divergence
Issue-Trace: opensearchserverless-collection-group-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearchserverless-collection-group-lifecycle
Issue-Shape-Id: com.amazonaws.opensearchserverless#OpenSearchServerless
Issue-Key: aoss|ListCollectionGroups|2xx->2xx|$.collectionGroupSummaries[arn="arn:aws:aoss:us-west-2:000000000000:collection-group/wugvc3isy6zdkzgfe60b"].storageConfiguration
Issue-Commit: 844d925c58d220632d295e47347c398f1151f4e4

# aoss ListCollectionGroups rows omit storageConfiguration that every collection-group detail reports

> _Note: this issue was identified against the code base at commit `844d925c58d220632d295e47347c398f1151f4e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCollectionGroups

## Description
ONE net-new wire cause from the replay of the `aoss` (OpenSearch Serverless) surface: the collection-group LISTING drops the `storageConfiguration` block that the recording shows on every row.

=== Cause 1 of 1: ListCollectionGroups omits storageConfiguration from every collectionGroupSummaries row ===

AWS: recorded behavior only for the listing row — Smithy: ListCollectionGroups -> 200 with `collectionGroupSummaries` (list of CollectionGroupSummary); smithy_query(shape="CollectionGroupSummary") models only id, arn, name, numberOfCollections, createdDate, capacityLimits, generation, so the member is not in the model as shipped. The same model carries `storageConfiguration` (target CollectionGroupStorageConfiguration, sole member `v2Enabled: Boolean`, "Whether the collection group's data is held on the next-generation (v2) storage stack") on CollectionGroupDetail and CreateCollectionGroupDetail as a documented LOCAL ADDITION, made "because the live OpenSearch Serverless API returns the block on every collection-group detail (recorded behaviour)". This fixture's recording shows the listing row carrying it as well: each `collectionGroupSummaries` entry has `storageConfiguration` = {"v2Enabled": <bool>} — `false` for a CLASSIC-generation group, which is the half a recording backs (the NEXTGEN -> true half is inference, as the crate's own doc states).

Emulator: the row has no `storageConfiguration` member at all — replay reports it `missing` (expected {'v2Enabled': False}, got None) on the group the fixture just created, with status 200 expected and 200 actual, so the status class and every other row member are right and only this block is absent. The local model addition was applied to the two detail shapes only, so CollectionGroupSummary models no such member and the listing row builder cannot set one even though the stored record already computes the value.

Why: a client that reads a group's storage mode off a listing — the cheap, paginated way to inspect every group in an account — cannot distinguish a v2-storage group from a classic one, and code that dereferences `summary.storageConfiguration.v2Enabled`, present on every real AWS row, faults on a null or silently defaults to the wrong storage stack. The listing also contradicts a read of the same group, which does report the block.

Fix: the model plus the ListCollectionGroups row builder — add to CollectionGroupSummary the same local-addition `storageConfiguration` member the two detail shapes already carry, and populate it from the stored record the way the detail renderers do, so a listing row and a read of the same group cannot disagree. Citations: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state.rs:1176-1196 (`CollectionGroupRecord::summary`, whose doc explicitly excludes storageConfiguration) against :1152 and :1172 (the create/read detail renderers, which set it) and :1198-1214 (`storage_configuration()`, which already derives `v2Enabled` from the stored `generation`); the handler is src/ops/list_collection_groups.rs.

Traces: opensearchserverless-collection-group-lifecycle#task#2.

NOTE on existing issues: the open issue 20261114T150008Z-aoss-6-net-new-wire-causes-collection-group.md describes the same ListCollectionGroups defect as its Cause 1, but its recorded replay key embeds a per-run collection-group ARN (collection-group/e0o7knckwjwgc9qzpdl3), so it does NOT match this run's key (collection-group/wugvc3isy6zdkzgfe60b) and the replay reported `owned_by` empty for this cluster — i.e. no open issue carries this cluster's replay key. The other five causes of that issue (GetAccessPolicy/DeleteAccessPolicy 400 after an idempotent-clientToken create, UpdateSecurityPolicy/GetSecurityPolicy policyVersion, CreateVpcEndpoint's VPC-absent message) were reported `owned_by` that issue in this run and are deliberately NOT re-filed here.

## Evidence
Replay: trace_set simparity-wire-aws-v1, emulator_up true (health 200, data listener accepts), 23 fixtures replayed, 150 steps, 142 passed, 6 mismatched, 2 blocked, score 94.7%.

This cluster: fixture opensearchserverless-collection-group-lifecycle, step task#2, service `aoss`, operation ListCollectionGroups, verdict `mismatch`, stub_path empty, owned_by empty, affected_count 1.

Detail, verbatim: `$.collectionGroupSummaries[arn="arn:aws:aoss:us-west-2:000000000000:collection-group/wugvc3isy6zdkzgfe60b"].storageConfiguration: missing (expected {'v2Enabled': False}, got None)`.

Expected status 200, actual status 200 — the divergence is the absent member only. The region, account and collection-group id inside the JSONPath key are per-run values that select the row; the requirement is that EVERY collectionGroupSummaries row carries a `storageConfiguration` object whose `v2Enabled` reports the group's storage stack (false for a CLASSIC-generation group), not any particular arn or id.

Model evidence: smithy_query(shape="CollectionGroupSummary") has no storageConfiguration member; smithy_query(shape="CollectionGroupDetail") has `storageConfiguration -> CollectionGroupStorageConfiguration`; smithy_query(shape="CollectionGroupStorageConfiguration") documents the shape as a LOCAL ADDITION grounded in recorded live behaviour.

## Steps to Reproduce
1. Build the emulator from this checkout and replay the corpus (replay_wire_corpus) — fixture opensearchserverless-collection-group-lifecycle, step task#2 mismatches.

2. Or exercise it directly against the `aoss` endpoint:
   a. CreateCollectionGroup(name=<group-name>) with no `generation` set (i.e. a CLASSIC-generation group). Observe the create's CreateCollectionGroupDetail DOES carry storageConfiguration {"v2Enabled": false}.
   b. GetCollectionGroup(... that group ...) — observe the detail also carries storageConfiguration {"v2Enabled": false}.
   c. ListCollectionGroups() and locate the row whose `arn`/`id` is that group.
      Expected (AWS / recording): the row carries `storageConfiguration`: {"v2Enabled": false}.
      Observed: the row has no `storageConfiguration` member at all, so the listing contradicts both the create and the read of the same group.
