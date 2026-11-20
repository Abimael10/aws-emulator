Issue-Source: trace-sweep-divergence
Issue-Trace: docdb-cluster-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: docdb-cluster-lifecycle
Issue-Shape-Id: com.amazonaws.docdb#AmazonRDSv19
Issue-Key: docdb|CreateDBCluster|2xx->2xx|$.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.EngineVersion
Issue-Key: docdb|DescribeDBClusters|2xx->2xx|$.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.EngineVersion
Issue-Related: 20261112T142502Z-docdb-cluster-endpoint-and-reader-endpoint-are-published.md
Issue-Commit: f8316590bcf97b65fd5e64b3c988acfd983e0aed

# DocDB CreateDBCluster defaults EngineVersion to the highest minor in its catalog instead of the engine's default version

> _Note: this issue was identified against the code base at commit `f8316590bcf97b65fd5e64b3c988acfd983e0aed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCluster

## Description
When CreateDBCluster is called without an explicit EngineVersion, the emulator stamps the cluster with the highest minor version present in its engine-version catalog rather than the engine's default version, and because that value is then persisted, every later read of the cluster reports it too. This issue bundles 2 causes (the create-time default and the read-back that inherits it). BOTH must be closed.

=== Cause 1 of 2: CreateDBCluster stamps the wrong default EngineVersion (fixture docdb-cluster-lifecycle, step task#7) ===
AWS: Smithy: CreateDBCluster (input CreateDBClusterMessage) -> CreateDBClusterResult.DBCluster; EngineVersion is an optional member documented as "The version number of the database engine to use. The --engine-version will default to the latest major engine version. For production workloads, we recommend explicitly declaring this parameter with the intended major engine version." The concrete default is recorded behavior only: with EngineVersion omitted, the recorded AWS CreateDBCluster response reports DBCluster.EngineVersion as the default version of the current major family — the x.y.0 release of that major (recorded as '5.0.0') — not the newest patch release the catalog lists for that family.
Emulator: the same request comes back with DBCluster.EngineVersion set to the highest minor/patch in its own catalog (recorded run: '5.0.2'), i.e. the default is computed as max-of-catalog instead of the engine's designated default version.
Why: a client that creates a cluster without pinning a version and then asserts or branches on the engine version it got (feature gating, parameter-group family selection, upgrade planning, infra drift detection) sees a version AWS would not have assigned. Anything that later compares the cluster's EngineVersion against a template or a DescribeDBEngineVersions default reports spurious drift, and an upgrade plan computed from it targets the wrong hop.
Fix: the CreateDBCluster handler's cluster-creation defaulting (the code that resolves an omitted EngineVersion from the engine-version catalog in the service state) must select the engine's default version for the major family, not the maximum entry in the catalog.
Traces: docdb-cluster-lifecycle#task#7. (The Endpoint/ReaderEndpoint host_not_own entries reported on this same step are a different, already-filed fault — open issue 20261112T142502Z-docdb-cluster-endpoint-and-reader-endpoint-are-published.md — and are NOT part of this issue.)

=== Cause 2 of 2: DescribeDBClusters reports the same wrong EngineVersion on read-back (fixture docdb-cluster-lifecycle, step task#8) ===
AWS: Smithy: DescribeDBClusters -> DBClusterMessage.DBClusters[].DBCluster.EngineVersion. A read-back must report the version the cluster was created with — the recorded DescribeDBClusters answer for the cluster created in the previous step reports the same default version AWS assigned at create time.
Emulator: DescribeDBClusters reports the wrong value the create step persisted, so the divergence survives the whole cluster lifecycle rather than being a create-response-only projection bug — which is why the fix belongs in the stored state, not only in the create serializer.
Why: a client that polls DescribeDBClusters after create (the normal availability-wait path) reads the wrong engine version for the lifetime of the cluster, so version-conditional behavior is wrong on every subsequent call, not just the create response.
Fix: whatever the create-time defaulting writes into the cluster record in the service state; once Cause 1 stores the engine's default version, this projection reports it. Verify both steps, since a fix to the serializer alone would leave the stored value wrong.
Traces: docdb-cluster-lifecycle#task#8. (The Endpoint/ReaderEndpoint host_not_own entries on this step are likewise covered by 20261112T142502Z-docdb-cluster-endpoint-and-reader-endpoint-are-published.md.)

No open DocDB issue covers the engine-version default: the open ones concern published endpoints, the DBInstance projection, 400 error-message text, subnet-group/EC2 resolution and DocDB Elastic.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (score 90.0%, 12 mismatched steps of 120):

Cause 1 — docdb-cluster-lifecycle#task#7, CreateDBCluster, expected 200 / actual 200:
  $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.EngineVersion: value_mismatch (expected '5.0.0', got '5.0.2')
  (same step also: DBCluster.Endpoint / DBCluster.ReaderEndpoint host_not_own — already filed, excluded from this issue)

Cause 2 — docdb-cluster-lifecycle#task#8, DescribeDBClusters, expected 200 / actual 200:
  $.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.EngineVersion: value_mismatch (expected '5.0.0', got '5.0.2')
  (same step also: DBCluster.Endpoint / DBCluster.ReaderEndpoint host_not_own — already filed, excluded from this issue)

The recorded '5.0.0' is the default version of the engine's current major family in that recording; '5.0.2' is the newest patch the emulator's catalog lists for the same family. The requirement is the defaulting rule (default version of the major family, as the engine-version catalog designates it), not the literal string from this run.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
2. Replay fixture docdb-cluster-lifecycle. At step task#7, CreateDBCluster is issued for a docdb cluster WITHOUT an EngineVersion parameter: the response's DBCluster.EngineVersion is the catalog's highest minor ('5.0.2') where the recorded AWS response has the engine's default version ('5.0.0').
3. At step task#8, DescribeDBClusters on that same cluster reports the same wrong EngineVersion, confirming the wrong value was persisted.
4. Both steps return HTTP 200; only the EngineVersion value (plus the separately filed endpoint hosts) diverges.
