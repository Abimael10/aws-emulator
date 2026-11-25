Issue-Source: trace-sweep-divergence
Issue-Trace: rds-aurora-serverless-data-api-managed-secret-sql-round-trip
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-aurora-serverless-data-api-managed-secret-sql-round-trip rds-aurora-serverless-v2-config-read-back rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found rds-db-cluster-lifecycle-with-snapshot rds-db-cluster-state-roles-and-capacity-operations rds-global-cluster-delete-with-member rds-multi-cluster-isolation
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBClusters|2xx->2xx|$.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.Endpoint
Issue-Key: rds|DescribeDBClusters|2xx->2xx|$.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.ReaderEndpoint
Issue-Key: rds|CreateDBCluster|2xx->2xx|$.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Endpoint
Issue-Key: rds|CreateDBCluster|2xx->2xx|$.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.ReaderEndpoint
Issue-Key: rds|DeleteDBCluster|2xx->2xx|$.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.Endpoint
Issue-Key: rds|DeleteDBCluster|2xx->2xx|$.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.ReaderEndpoint
Issue-Related: 20261108T233943Z-rds-db-cluster-envelopes-report-the-emulator-s-random.md
Issue-Commit: 28e13556b346bec2a9903044f83d66021b33e840

# rds DB cluster envelopes publish the emulator's ephemeral listener port and an unserved endpoint host

> _Note: this issue was identified against the code base at commit `28e13556b346bec2a9903044f83d66021b33e840`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
THREE net-new causes, all one shared fault: every DB-cluster envelope the rds emulator serializes (read, create and delete) fills DBCluster.Port with the emulator's own randomly-chosen data listener port and DBCluster.Endpoint / DBCluster.ReaderEndpoint with a host the emulator does not serve, instead of the engine's connection port and a cluster endpoint host that can actually be reached. Each operation is counted separately below because each has its own serializer path and its own replay binding; a fix must close all three.

Cause 1 of 3 — DescribeDBClusters (fixture rds-aurora-serverless-data-api-managed-secret-sql-round-trip, step task#9).
AWS: Smithy rds (com-amazonaws-rds-amazonrdsv19) DBCluster.Port — "The port that the database engine is listening on"; CreateDBCluster.Port — "The port number on which the instances in the DB cluster accept connections. Valid Values: 1150-65535. Default: RDS for MySQL and Aurora MySQL - 3306; RDS for PostgreSQL and Aurora PostgreSQL - 5432". DBCluster.Endpoint is "The connection endpoint for the primary instance of the DB cluster" and DBCluster.ReaderEndpoint is the load-balancing reader endpoint. A described aurora-postgresql cluster therefore reports Port 5432 (or whatever Port the create request asked for) and writer/reader endpoint hosts that resolve to something serving the engine.
Emulator: DescribeDBClusters reports $.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.Port = '40409' where 5432 is required — the value tracks the emulator's ephemeral listener port, not the cluster's engine port — and publishes Endpoint / ReaderEndpoint on a '<cluster>.cluster[-ro]-<id>.<region>.rds.localhost' host that the replay flags host_not_own, i.e. a name the emulator does not answer on.
Why: a client reads the cluster's Port and endpoint out of DescribeDBClusters to build its connection string. With a port that is an internal implementation detail and a host nothing serves, every native-protocol connection attempt fails, and read/write splitting via the reader endpoint is unaddressable.
Fix: the DescribeDBClusters projection of cluster state — the DBCluster serializer and the cluster state that stores port and endpoint hostnames (set the engine/requested port at create time and publish endpoint hosts the emulator actually serves).
Traces: rds-aurora-serverless-data-api-managed-secret-sql-round-trip#task#9; also rds-aurora-serverless-v2-config-read-back#task#2, rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#6 and #task#9 (4 steps in the cluster).

Cause 2 of 3 — CreateDBCluster (fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found, step task#5).
AWS: same model members; for an aurora-mysql cluster created without an explicit Port the created DBCluster reports Port 3306, and Endpoint/ReaderEndpoint are reachable cluster endpoint hosts.
Emulator: the 200 CreateDBCluster response carries $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Port = '44683' (expected '3306') and Endpoint / ReaderEndpoint on the same unserved '.rds.localhost' host (host_not_own).
Why: the create response is where a provisioning client caches the cluster's connection coordinates; it caches an unusable port and an unresolvable host and never retries.
Fix: the CreateDBCluster handler/serializer and the cluster state it writes — derive Port from the request's Port or the engine default and mint endpoint hostnames the emulator serves.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#5; also rds-db-cluster-lifecycle-with-snapshot#task#1, rds-db-cluster-state-roles-and-capacity-operations#setup#1, rds-global-cluster-delete-with-member#task#7, rds-multi-cluster-isolation#setup#1 and #setup#2 (6 steps in the cluster).

Cause 3 of 3 — DeleteDBCluster (fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found, step task#19).
AWS: DeleteDBCluster returns the same DBCluster shape, so the deleting cluster's Port and Endpoint/ReaderEndpoint match what the cluster was created with (3306 for aurora-mysql here).
Emulator: $.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.Port = '44683' (expected '3306') and Endpoint / ReaderEndpoint again on the unserved '.rds.localhost' host (host_not_own).
Why: a client that diffs the delete envelope against the cluster it created sees the connection coordinates change under it, and tooling that keys off Port/Endpoint cannot correlate the deleted cluster with the one it provisioned.
Fix: the DeleteDBCluster response projection — it must serialize the stored cluster port and endpoint hosts, which is the same state fix as causes 1 and 2.
Traces: rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found#task#19; also rds-db-cluster-state-roles-and-capacity-operations#teardown#13 (2 steps in the cluster).

Related but NOT covering these wire steps: open issues 20261121T053640Z-logs-rds-an-available-aurora-postgresql-cluster-publishes and 20261121T064921Z-rds-s3-3-net-new-causes-an-aurora describe the unserved '.rds.localhost' writer endpoint behaviorally, and 20261118T210044Z-rds-2-net-new-causes-describedbclusters-publishes-an covers identical writer/reader endpoints; none of them carries the replay key for these three clusters, which the replay reports as unowned.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200):

1) DescribeDBClusters, expected 200 / actual 200, fixture rds-aurora-serverless-data-api-managed-secret-sql-round-trip step task#9 (cluster of 4 steps):
   $.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.Port: value_mismatch (expected '5432', got '40409')
   $.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.Endpoint: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-rds-eae6f5af.cluster-32wknz53ptu9.us-west-2.rds.localhost')
   $.DescribeDBClustersResponse.DescribeDBClustersResult.DBClusters.DBCluster.ReaderEndpoint: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-rds-eae6f5af.cluster-ro-32wknz53ptu9.us-west-2.rds.localhost')

2) CreateDBCluster, expected 200 / actual 200, fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found step task#5 (cluster of 6 steps):
   $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Port: value_mismatch (expected '3306', got '44683')
   $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.Endpoint: host_not_own (expected '<shaped-not-asserted>', got 'sim2real-probe-rds-cl-b85f63b1.cluster-ruplogumjje4.us-west-2.rds.localhost')
   $.CreateDBClusterResponse.CreateDBClusterResult.DBCluster.ReaderEndpoint: host_not_own (got 'sim2real-probe-rds-cl-b85f63b1.cluster-ro-ruplogumjje4.us-west-2.rds.localhost')

3) DeleteDBCluster, expected 200 / actual 200, fixture rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found step task#19 (cluster of 2 steps):
   $.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.Port: value_mismatch (expected '3306', got '44683')
   $.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.Endpoint: host_not_own (got 'sim2real-probe-rds-cl-b85f63b1.cluster-ruplogumjje4.us-west-2.rds.localhost')
   $.DeleteDBClusterResponse.DeleteDBClusterResult.DBCluster.ReaderEndpoint: host_not_own (got 'sim2real-probe-rds-cl-b85f63b1.cluster-ro-ruplogumjje4.us-west-2.rds.localhost')

The requirement is the SHAPE, not these recorded values: cluster names, the random cluster-resource-id segment ('32wknz53ptu9', 'ruplogumjje4') and the emulator's listener number are per-run. What must hold is that Port is the engine's connection port (the request's Port, else 3306 for aurora-mysql / 5432 for aurora-postgresql) and that Endpoint/ReaderEndpoint are hosts the emulator answers on, with the writer host and the 'cluster-ro-' reader host distinct.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 2: replay rds-db-cluster-aurora-mysql-provider-explicit-flags-one-writer-tags-and-not-found step task#5 — CreateDBCluster for an aurora-mysql cluster with no explicit Port. Read DBCluster.Port (3306 expected, emulator returns its ephemeral listener port) and DBCluster.Endpoint / ReaderEndpoint (a '.rds.localhost' host the emulator does not serve).
3. Cause 1: replay rds-aurora-serverless-data-api-managed-secret-sql-round-trip step task#9 — DescribeDBClusters on the aurora-postgresql serverless cluster and read DBClusters.DBCluster.Port (5432 expected) plus Endpoint / ReaderEndpoint.
4. Cause 3: replay the same mysql fixture step task#19 — DeleteDBCluster and read DeleteDBClusterResult.DBCluster.Port / Endpoint / ReaderEndpoint; they repeat the create-time divergence.
5. Confirm the endpoint host is unreachable by opening a TCP connection to the published Endpoint host on the published Port.
