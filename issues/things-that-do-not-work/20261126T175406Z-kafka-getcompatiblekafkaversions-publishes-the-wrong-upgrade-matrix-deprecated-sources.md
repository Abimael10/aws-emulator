Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-catalog-get-compatible-kafka-versions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-catalog-get-compatible-kafka-versions
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|GetCompatibleKafkaVersions|2xx->2xx|$.compatibleKafkaVersions[*]
Issue-Commit: ff3af10df4d8dabe5b2ee5f578577b6467ef3fb3

# GetCompatibleKafkaVersions publishes the wrong upgrade matrix: deprecated sources carry an empty targetVersions and the row sequence disagrees with the published table

> _Note: this issue was identified against the code base at commit `ff3af10df4d8dabe5b2ee5f578577b6467ef3fb3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCompatibleKafkaVersions

## Description
Cause 1 of 1: the whole-catalog form of GetCompatibleKafkaVersions (GET /v1/compatible-kafka-versions with NO clusterArn) answers 200 with a compatibleKafkaVersions table whose rows disagree with the one AWS publishes, in both which sources appear and what each source's targetVersions holds. The emulator derives every row's target list from its own NEW-CLUSTER offering and suppresses the list entirely for any source it no longer offers, so every DEPRECATED source is published as terminal (targetVersions: []), and it emits one row per catalog row in ascending-release order, which is not the sequence the service publishes.

AWS: Smithy (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json): GetCompatibleKafkaVersions GET /v1/compatible-kafka-versions, success 200, input ClusterArn (__string, httpQuery clusterArn, OPTIONAL), output CompatibleKafkaVersions -> __listOfCompatibleKafkaVersion (jsonName compatibleKafkaVersions) of CompatibleKafkaVersion{SourceVersion (jsonName sourceVersion), TargetVersions (jsonName targetVersions, __listOf__string)}; doc "Gets the Apache Kafka versions to which you can update the MSK cluster." The model constrains neither the row set nor the target values, so WHICH rows the matrix carries is recorded behavior only. The recording shows the published matrix is MSK's upgrade table: a row's targetVersions is the set of versions a cluster running that sourceVersion may be updated to, and that set is NON-EMPTY for versions MSK has deprecated — publishing an upgrade path off a deprecated version is the whole point of the table, since that is how a cluster still running one is moved forward. The recorded rows show this directly: sourceVersion 2.4.1.1, 3.1.1, 3.2.0, 3.3.1, 3.3.2, 3.4.0, 3.5.1 and 2.1.0 each carry a non-empty ladder (recorded as ['3.6.0', '3.7.x', '3.8.x', '3.9.x']), the KRaft lines carry their own ('3.9.x.kraft' -> ['4.1.x.kraft', '4.0.x.kraft']; '3.7.x.kraft' -> ['4.1.x.kraft', '3.8.x.kraft', '3.9.x.kraft', '4.0.x.kraft']), and 3.6.0 -> ['3.7.x', '3.8.x', '3.9.x']. The requirement is the rule, not these literal strings: a source the service publishes an upgrade path off must be published WITH that path, and the rows must be the sequence the service publishes rather than a re-sort of the local catalog.

Emulator: the handler builds the table from crate::state::published_kafka_versions() (one row per KAFKA_VERSION_CATALOG row, ascending by release, duplicates included) and fills each row with crate::state::compatible_target_versions(source), which returns an EMPTY vector whenever !is_offered_kafka_version(source) and otherwise filters the service's narrow new-cluster offering. The replayed answer is therefore 200 with rows [{sourceVersion '1.1.1', targetVersions []}, {'2.1.0', []}, {'2.2.1', []}, {'2.3.1', []}, {'2.4.1', []}, {'2.4.1.1', []}, {'2.5.1', []}, {'2.6.0', []}, {'2.6.1', []}, {'2.6.2', []}, {'2.6.3', []}, ... {'2.7.1', []}, {'2.7.2', []}, ...] — every deprecated source terminal, and the row at each index naming a different source than the published table does (index 1 is {'2.1.0', []} where the service publishes {'3.9.x.kraft', ['4.1.x.kraft', '4.0.x.kraft']}; index 5 is {'2.4.1.1', []} where the service publishes {'2.4.1.1', ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — same source, emptied ladder). Status, content type and shape are all correct; only the table's content diverges.

Why: this call IS the upgrade-planning API. A client that enumerates the matrix to find where a cluster running a deprecated Apache Kafka version can go is told there is NO upgrade target at all for it, so an upgrade planner, a drift remediation job or a console view concludes the cluster is stranded and never issues the UpdateClusterKafkaVersion the real service would accept. A client that indexes or zips the matrix against ListKafkaVersions also reads the wrong source at every position, because the row sequence is a local re-sort rather than the published one.

Fix: the upgrade-ladder derivation in the kafka service state and the matrix projection that reads it — compatible_target_versions must publish the ladder off a source the service still publishes a path for, instead of short-circuiting to an empty list for any source that is not in the new-cluster offering, and the published-table path must emit the service's published row sequence rather than the catalog's ascending order. The per-cluster form (clusterArn given) shares the same derivation and must move with it.

Traces: kafka-catalog-get-compatible-kafka-versions#task#1. Code: services/service-com-amazonaws-kafka-kafka/src/ops/get_compatible_kafka_versions.rs lines 253-274 (rows from published_kafka_versions(), targets from compatible_target_versions) and services/service-com-amazonaws-kafka-kafka/src/state/kafka_versions.rs lines 294-319 (compatible_target_versions, the !is_offered_kafka_version early return at 295-297), 250-263 (upgrade_target_kafka_versions, keyed off KAFKA_VERSIONS_FOR_A_NEW_CLUSTER) and 200-208 (published_kafka_versions).

Related open issues that do NOT cover this cause: 20261121T113417Z-kafka-createcluster-createclusterv2-refuse-active-kafka-versions-the-service.md concerns the CreateCluster/CreateClusterV2 version-admission gate, not the matrix this operation publishes, and carries none of this fixture's replay keys; 20261111T115530Z / 20261115T194344Z style catalog-row-duplication issues are about ListKafkaVersions rows, not the target lists.

## Evidence
Fixture kafka-catalog-get-compatible-kafka-versions, step task#1 — GET /v1/compatible-kafka-versions (no clusterArn). Expected status 200, actual status 200; the divergence is entirely in $.compatibleKafkaVersions.

Expected (AWS, recorded) vs actual (emulator), element by element as the replay reported them:
  $.compatibleKafkaVersions[1]: expected {'sourceVersion': '3.9.x.kraft', 'targetVersions': ['4.1.x.kraft', '4.0.x.kraft']} — got {'sourceVersion': '2.1.0', 'targetVersions': []}
  [4]: expected {'sourceVersion': '3.7.x.kraft', 'targetVersions': ['4.1.x.kraft', '3.8.x.kraft', '3.9.x.kraft', '4.0.x.kraft']} — got {'sourceVersion': '2.4.1', 'targetVersions': []}
  [5]: expected {'sourceVersion': '2.4.1.1', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.4.1.1', 'targetVersions': []}
  [6]: expected {'sourceVersion': '3.3.2', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.5.1', 'targetVersions': []}
  [7]: expected {'sourceVersion': '3.5.1', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.6.0', 'targetVersions': []}
  [8]: expected {'sourceVersion': '3.6.0', 'targetVersions': ['3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.6.1', 'targetVersions': []}
  [9]: expected {'sourceVersion': '3.1.1', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.6.2', 'targetVersions': []}
  [10]: expected {'sourceVersion': '3.2.0', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.6.3', 'targetVersions': []}
  [12]: expected {'sourceVersion': '3.3.1', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.7.1', 'targetVersions': []}
  [13]: expected {'sourceVersion': '3.4.0', 'targetVersions': ['3.6.0', '3.7.x', '3.8.x', '3.9.x']} — got {'sourceVersion': '2.7.2', 'targetVersions': []}
  [14]: expected {'sourceVersion': '2.1.0', 'targetVersions': ['3.6.0', '3.7.x', ...]} — got a later catalog row with 'targetVersions': []
The replay's divergence key lists mismatching indices 1-10, 12-29, 31, 32, 34 and 36, i.e. nearly every row of the table.

Two things are wrong, and they are the same derivation: (a) a source the service publishes an upgrade path off is published with 'targetVersions': [] whenever the emulator no longer offers that version to a NEW cluster — visible at index 5, where source and expectation agree on '2.4.1.1' and only the ladder is emptied; (b) the row at a given index names a different source, because the emulator emits its catalog in ascending-release order instead of the published row sequence.

No per-run value is involved: there is no id, timestamp, host or request id anywhere in this payload — the whole response is the service's static published upgrade table. The version strings above are the recorded instance of the rule ("a published source carries the upgrade path the service publishes off it"), not themselves the requirement.

## Steps to Reproduce
1. Boot the emulator from this checkout (the kafka service answers; /__admin/health is 200).
2. Call GetCompatibleKafkaVersions with NO ClusterArn: GET /v1/compatible-kafka-versions (`aws kafka get-compatible-kafka-versions`).
3. Read $.compatibleKafkaVersions. Observe that every row whose sourceVersion the catalog publishes as DEPRECATED carries "targetVersions": [] — e.g. {"sourceVersion": "2.4.1.1", "targetVersions": []} — where the service publishes {"sourceVersion": "2.4.1.1", "targetVersions": ["3.6.0", "3.7.x", "3.8.x", "3.9.x"]}.
4. Compare the row sequence index by index against the published table: index 1 is {"sourceVersion": "2.1.0", "targetVersions": []} where the service publishes {"sourceVersion": "3.9.x.kraft", "targetVersions": ["4.1.x.kraft", "4.0.x.kraft"]}, and the sources disagree at nearly every index thereafter.
5. Cross-check the same derivation on the per-cluster form: create a provisioned cluster, then GET /v1/compatible-kafka-versions?clusterArn=<arn> for a cluster running a deprecated version — the single row comes back with an empty targetVersions as well.
6. Re-replay fixture kafka-catalog-get-compatible-kafka-versions, step task#1, to confirm the fix.
