Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found rds-db-cluster-lifecycle-with-snapshot rds-db-cluster-state-roles-and-capacity-operations rds-global-cluster-delete-with-member rds-multi-cluster-isolation rds-aurora-serverless-data-api-managed-secret-sql-round-trip rds-aurora-serverless-v2-config-read-back
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|CreateDBCluster|2xx->2xx|$.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Endpoint
Issue-Key: rds|CreateDBCluster|2xx->2xx|$.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.ReaderEndpoint
Issue-Key: rds|DescribeDBClusters|2xx->2xx|$.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.Endpoint
Issue-Key: rds|DescribeDBClusters|2xx->2xx|$.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.ReaderEndpoint
Issue-Key: rds|DeleteDBCluster|2xx->2xx|$.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.Endpoint
Issue-Key: rds|DeleteDBCluster|2xx->2xx|$.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.ReaderEndpoint
Issue-Related: 20261108T233943Z-rds-db-cluster-envelopes-report-the-emulator-s-random.md
Issue-Commit: 0a2817577611e152839e8496ef0accae0bb6c9d7

# rds DB cluster envelopes publish an ephemeral listener port and an unserved endpoint host on create, describe and delete

> _Note: this issue was identified against the code base at commit `0a2817577611e152839e8496ef0accae0bb6c9d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCluster

## Description
Every DB-cluster envelope the emulator serializes carries DBCluster.Port = the emulator's own OS-assigned data listener port, and DBCluster.Endpoint / DBCluster.ReaderEndpoint on a host the emulator does not answer on. The same defect shows on three separate operations, which is 3 causes in this bundle.

Cause 1 of 3: CreateDBCluster.
AWS: Smithy: CreateDBClusterMessage$Port documents 'The port number on which the instances in the DB cluster accept connections. Valid Values: 1150-65535. Default: RDS for MySQL and Aurora MySQL - 3306; RDS for PostgreSQL and Aurora PostgreSQL - 5432'. So when the request omits Port, the created cluster's Port is the engine default (3306 for aurora-mysql, 5432 for aurora-postgresql); when the request supplies Port, that value is echoed. Smithy: DBCluster$Endpoint is 'The connection endpoint for the primary instance of the DB cluster' — AWS publishes it as <cluster-identifier>.cluster-<shard>.<region>.rds.amazonaws.com and the reader as <cluster-identifier>.cluster-ro-<shard>.<region>.rds.amazonaws.com, on a host that actually accepts connections. The fixture asserts the endpoint's shape and resolvability rather than a literal host (expected '<shaped-not-asserted>'), and asserts the port value exactly.
Emulator: CreateDBCluster returns 200 with DBCluster.Port set to the emulator's ephemeral listener port (replay recorded 45179 against an expected 3306) and publishes Endpoint / ReaderEndpoint under a '.rds.localhost' suffix — a host the replayer flags host_not_own, i.e. an address other than the one the emulator is reached on.
Why: a client takes (Endpoint, Port) straight from the create response and dials it. A '.rds.localhost' name does not resolve and the announced port is not the engine port the client's driver expects, so every connection attempt from the create response fails and no cluster-port or connection scenario can proceed.
Fix: the CreateDBCluster handler / cluster-state construction must derive Port from the request's Port or the engine default, and the DB-cluster endpoint serializer must publish the address the emulator is actually reachable on, in AWS's cluster-endpoint shape.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#5 (also rds-db-cluster-lifecycle-with-snapshot#task#1, rds-db-cluster-state-roles-and-capacity-operations#setup#1, rds-global-cluster-delete-with-member#task#7, rds-multi-cluster-isolation#setup#1, rds-multi-cluster-isolation#setup#2).

Cause 2 of 3: DescribeDBClusters reads back the same wrong port and host.
AWS: as above — DescribeDBClusters reports the cluster's stored Port (5432 for an aurora-postgresql cluster) and its resolvable cluster/reader endpoints.
Emulator: DescribeDBClusters answers 200 with DBClusters.DBCluster.Port = the ephemeral listener port (recorded 38663 where 5432 was expected) and Endpoint/ReaderEndpoint on '.rds.localhost' (host_not_own).
Why: a client that discovers a cluster rather than creating it is given the same unusable address/port pair, so the read-back path is wrong independently of create.
Fix: the DescribeDBClusters serializer must render the stored engine port and the served endpoint host, not the listener's.
Traces: rds-aurora-serverless-data-api-managed-secret-sql-round-trip#task#9 (also rds-aurora-serverless-v2-config-read-back#task#2, rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#6 and #task#9).

Cause 3 of 3: DeleteDBCluster's returned envelope.
AWS: DeleteDBCluster returns the DBCluster envelope of the cluster being deleted, with the same Port and endpoint members it reported while available.
Emulator: DeleteDBCluster answers 200 with Port = the ephemeral listener port (recorded 45179 where 3306 was expected) and Endpoint/ReaderEndpoint on '.rds.localhost'.
Why: a client that records the final envelope for audit or that re-reads it to confirm the deleted cluster's configuration sees a port and host that never described the cluster.
Fix: same envelope serializer as causes 1-2 — the delete path must not re-synthesize the port/host either.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#19 (also rds-db-cluster-state-roles-and-capacity-operations#teardown#13).

Related open issues cover the DB *instance* envelope and behavioral endpoint checks (20261122T000359Z-rds-db-db-instance-envelopes..., 20261123T041052Z-rds-4-net-new-endpoint-publication-causes-a.md, 20261121T053640Z-logs-rds-...); none of them carries these three cluster-envelope wire replay keys, so they do not cover this finding.

## Evidence
CreateDBCluster (rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#5): expected status 200 / actual 200, but $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Port: expected '3306', got '45179'; .Endpoint: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-rds-cl-dfe1a5bf.cluster-spx5zprbbj30.us-west-2.rds.localhost'); .ReaderEndpoint: host_not_own (got 'sim2real-probe-rds-cl-dfe1a5bf.cluster-ro-spx5zprbbj30.us-west-2.rds.localhost'). 6 steps affected.
DescribeDBClusters (rds-aurora-serverless-data-api-managed-secret-sql-round-trip#task#9): $...DBClusters.DBCluster.Port: expected '5432', got '38663'; .Endpoint / .ReaderEndpoint host_not_own on '...cluster-0xosdf6ot2uu.us-west-2.rds.localhost'. 4 steps affected.
DeleteDBCluster (rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#19): $.DeleteDBClusterResponse...DBCluster.Port: expected '3306', got '45179'; .Endpoint / .ReaderEndpoint host_not_own. 2 steps affected.
The recorded port values (3306/5432) are the modeled engine defaults; the actual values (45179/38663) differ per boot, which is exactly the signature of the OS-assigned listener port being published.

## Steps to Reproduce
1. Replay fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found step task#5: CreateDBCluster for an aurora-mysql cluster with no Port in the request, and compare DBCluster.Port (expect 3306) and DBCluster.Endpoint/ReaderEndpoint host.
2. Replay rds-aurora-serverless-data-api-managed-secret-sql-round-trip step task#9: DescribeDBClusters on the aurora-postgresql cluster and compare Port (expect 5432).
3. Replay rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found step task#19: DeleteDBCluster and compare the returned DBCluster.Port and endpoints.
4. In each case confirm the published endpoint host is one the emulator actually accepts connections on.
