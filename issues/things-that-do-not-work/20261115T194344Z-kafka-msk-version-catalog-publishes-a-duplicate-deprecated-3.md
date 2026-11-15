Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-catalog-list-kafka-versions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-catalog-list-kafka-versions
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|ListKafkaVersions|2xx->2xx|$.kafkaVersions[*]
Issue-Commit: d16a934b13e42144c8b5a8031e6a8b5e40ca2e53

# MSK version catalog publishes a duplicate DEPRECATED 3.9.x row where MSK publishes 3.8.link

> _Note: this issue was identified against the code base at commit `d16a934b13e42144c8b5a8031e6a8b5e40ca2e53`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListKafkaVersions

## Description
ONE net-new root cause on the kafka (Amazon MSK) wire surface. The replay reported this cluster with an EMPTY stub_path and an EMPTY owned_by: no open issue carries its replay key.

Cause 1 of 1 - ListKafkaVersions' published version catalog carries a SECOND, DEPRECATED row for a version it already published as ACTIVE, in the position where MSK publishes a different version's DEPRECATED row, and omits that version from the catalog entirely.

AWS: Smithy kafka#ListKafkaVersions (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json): GET /v1/kafka-versions, HTTP 200 -> ListKafkaVersionsResponse.KafkaVersions (jsonName kafkaVersions), a list of KafkaVersion{Version (jsonName version), Status (jsonName status, enum KafkaVersionStatus = ACTIVE | DEPRECATED)}; paginated by NextToken/MaxResults (MaxResults @range 1..100). The model enumerates no concrete versions and states no ordering, so WHICH versions the catalog holds and WHICH status each carries is recorded behavior only. The recorded behavior is: MSK publishes one row per Apache Kafka version it has offered, each version appearing EXACTLY ONCE in the page, in the service's own fixed published order, and the row at the asserted position is version '3.8.link' with status DEPRECATED. That '3.8.link' is a real row of this service's catalog and not a per-run artifact is corroborated inside the same corpus: the recorded GetCompatibleKafkaVersions matrix publishes sourceVersion '3.8.link' with an empty targetVersions list. The requirement is the rule, not one run's literal strings: the page must hold the versions the service offers, one row per version, each with the status the service actually honours for it, so the ordinary client read (fold the page into a version -> status map, last row per version winning) agrees with the gates the same service applies on CreateCluster/CreateClusterV2/CreateConfiguration.

Emulator: HTTP 200 with a page of the same length and the same rows everywhere else, but at the asserted position it serves {"version": "3.9.x", "status": "DEPRECATED"} - a SECOND row for 3.9.x, which the same page already published as ACTIVE earlier - and it publishes NO '3.8.link' row at all. So the catalog both duplicates a version under a conflicting status and drops a version MSK publishes.

Why: ListKafkaVersions is how a client discovers which version it may ask for, and it is read as a version -> status mapping. A client folding the emulator's page concludes that 3.9.x - the version the emulator's own create gates accept, because those gates read the de-duplicated version set - is DEPRECATED, so a version picker refuses to create a cluster or steers onto a version the service would refuse; a client that instead takes the FIRST row per version sees a catalog that disagrees with itself. At the same time '3.8.link' is invisible, so a client cannot discover or select a version the service publishes, and version-planning tooling that diffs the whole catalog against MSK's sees the wrong rows at the wrong positions. No status or error code diverges: the defect is entirely in the body's rows.

Fix: the published catalog rows in the kafka crate's state module - crate::state::KAFKA_VERSION_CATALOG, paged by crate::state::kafka_version_page and projected row-for-row by the handler (services/service-com-amazonaws-kafka-kafka/src/ops/list_kafka_versions.rs, the mapping at lines 205-217, which copies each catalog entry's version/status verbatim and therefore cannot be the source of the wrong value). The catalog must publish '3.8.link' DEPRECATED at that position and must not publish any version twice, so that the page agrees with the offered-version predicate the create/update gates read (crate::state::catalogued_kafka_versions) and with the matrix GetCompatibleKafkaVersions serves.

Related open issues that do NOT cover this replay key (the replay marked this cluster unowned): 20261111T015651Z-kafka-listkafkaversions-publishes-3-9-x-s-deprecated.md and 20261111T115530Z-kafka-listkafkaversions-publishes-a-second-deprecated-3-9.md report the client-visible CONSEQUENCE of the duplicate 3.9.x row from behavioral checks, but neither names this fixture step nor the missing '3.8.link' row; 20261111T034904Z-kafka-msk-compatible-version-matrix-and-rejectclientvpcconnection-refusal-diverge.md covers GetCompatibleKafkaVersions' matrix, a different operation and a different replay key. A correction to the shared catalog should close all of them, but this work item is unowned.

Traces: kafka-catalog-list-kafka-versions#task#1 (ListKafkaVersions, 200 vs 200, body mismatch on $.kafkaVersions[29]).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator up (/__admin/health 200, data listener accepting), 50 fixtures replayed, 163 steps, 131 passed, 32 mismatched, score 80.4%, 0 blocked, 0 errored.

Cluster: fixture kafka-catalog-list-kafka-versions, step task#1, operation ListKafkaVersions, verdict mismatch, stub_path empty, owned_by empty, affected_count 1.
  status:            expected 200, got 200 (no divergence)
  $.kafkaVersions[29]: expected {'version': '3.8.link', 'status': 'DEPRECATED'}, got {'version': '3.9.x', 'status': 'DEPRECATED'}

That is the ONLY asserted position that diverges, so the page's length and every other row already line up: the emulator has substituted a second 3.9.x DEPRECATED row for MSK's 3.8.link DEPRECATED row, which both duplicates 3.9.x (published ACTIVE earlier in the same page) and leaves 3.8.link absent from the catalog.

Corroboration from the same corpus that '3.8.link' is a catalog version rather than a per-run value: the recorded GetCompatibleKafkaVersions response asserts a row {'sourceVersion': '3.8.link', 'targetVersions': []} (fixture kafka-catalog-get-compatible-kafka-versions, step task#1, index 3), where the emulator serves {'sourceVersion': '2.3.1', ...}.

The version strings and statuses here are the service's published catalog contents, which is the requirement. Nothing in this step depends on a per-run value: no ARN, request id, timestamp, host or generated name appears in the asserted rows.

## Steps to Reproduce
1. Boot the emulator built from this checkout (admin health 200, data listener accepting) and replay trace set simparity-wire-aws-v1.
2. Call ListKafkaVersions with no parameters: `aws kafka list-kafka-versions` (GET /v1/kafka-versions). Expect HTTP 200 and MSK's catalog, each version appearing exactly once, with a row {version: '3.8.link', status: 'DEPRECATED'} at the position asserted by the recording (array index 29 of the returned kafkaVersions).
3. Observe instead a second {version: '3.9.x', status: 'DEPRECATED'} row at that position - 3.9.x having already been published ACTIVE earlier in the same page - and no '3.8.link' row anywhere in the response.
4. Fold the page the way a client does, `{v['Version']: v['Status'] for v in resp['KafkaVersions']}`: '3.9.x' reads 'DEPRECATED', while CreateClusterV2 with Provisioned.KafkaVersion='3.9.x' is still accepted by the same emulator, and '3.8.link' is absent from the map.
5. Re-replay kafka-catalog-list-kafka-versions#task#1 to confirm the fix.
