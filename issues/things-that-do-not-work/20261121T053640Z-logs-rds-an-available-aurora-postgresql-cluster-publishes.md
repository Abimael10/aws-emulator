Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__rds.obs-rds-3-aurora-postgresql-log-export-error-metric-filter-counts-only-error-lines
Issue-Key: logs__rds.obs-rds-3-aurora-postgresql-log-export-error-metric-filter-counts-only-error-lines|run#1|PublishedUrlRejected|host_dial, port_dial = ctx.endpoint_host(host, int(port))
Issue-Commit: a3f33f7e41c0f82edda298cff5ad8a1ec31b20ba

# logs__rds: an available aurora-postgresql cluster publishes a writer Endpoint on an unserved '.rds.localhost' host, so no client can connect and the postgresql log-export metric-filter flow is ungradeable

> _Note: this issue was identified against the code base at commit `a3f33f7e41c0f82edda298cff5ad8a1ec31b20ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
Cause 1 of 1: the writer endpoint DescribeDBClusters reports for an 'available' Aurora PostgreSQL cluster is a cluster-domain hostname under '.rds.localhost' that the emulator neither serves nor resolves, so the advertised connection endpoint cannot be dialled.

AWS: Smithy: rds DBCluster.Endpoint — "The connection endpoint for the primary instance of the DB cluster" (with DBCluster.Port). On AWS the value is a resolvable DNS name that accepts a PostgreSQL connection once Status is 'available'; the fixed parts are the '.cluster-' infix and the regional 'rds.amazonaws.com' suffix, and the variable parts are the DBClusterIdentifier, the account/region-specific cluster hash and the region — all taken from the request and the service's state. What is structural, and what the check depends on, is that the announced Endpoint:Port is an address the responding service actually answers on.

Emulator: DescribeDBClusters answers 200 with Status 'available' and an Endpoint of the form '<db-cluster-identifier>.cluster-<hash>.<region>.rds.localhost' (one run: 'sim2real-xsvc-aurlog-bb58d39fe443.cluster-g80zhhzgge6x.us-west-2.rds.localhost', Port 38645 — identifier, hash, region and port are per-run values, not the requirement). That host is not one of the emulator's own hosts, so the published-endpoint guard refuses it: "endpoint_host was given <host>:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address". No status or error-code divergence — the read succeeds, the published value is unusable.

Why: a client that does the documented thing — connect to Endpoint:Port once the cluster is available — cannot reach the database at all, so nothing downstream of the connection exists: no SQL session, no RAISE LOG/ERROR line, no postgresql log export into the CloudWatch Logs group, and no metric-filter count. Every data-plane guarantee of an Aurora cluster is unreachable from the API's own answer.

Fix: the RDS DBCluster endpoint minting/serializer path (the component that computes Endpoint/ReaderEndpoint/Port from cluster state) must publish an authority the emulator answers on — either an own host (or loopback address) for the cluster's listener, or the cluster-domain name additionally made resolvable and served by the emulator. Related open issues describe neighbouring shape defects in the same values but do not carry this check: 20261111T194847Z-rds-7-net-new-causes-cluster-writer-reader.md (endpoints not cluster-domain hostnames) and 20261116T052754Z-rds-secretsmanager-the-aurora-production-readiness-audit-cannot-derive-read.md (ReaderEndpoint is a literal host:port).

Traces: logs__rds.obs-rds-3-aurora-postgresql-log-export-error-metric-filter-counts-only-error-lines.

## Evidence
Expected: an 'available' aurora-postgresql cluster reports an Endpoint/Port pair that resolves and accepts a PostgreSQL connection from a client of the emulator. Actual: simparity.published.PublishedUrlRejected: "endpoint_host was given sim2real-xsvc-aurlog-bb58d39fe443.cluster-g80zhhzgge6x.us-west-2.rds.localhost:38645 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address". The check had already passed the EnabledCloudwatchLogsExports == ['postgresql'] assertion, so the divergence is solely the announced endpoint host.

## Steps to Reproduce
1. CreateDBCluster with Engine aurora-postgresql and EnableCloudwatchLogsExports ['postgresql'], plus a writer instance; wait for Status 'available'.
2. Read DBCluster.Endpoint and DBCluster.Port from DescribeDBClusters.
3. Try to resolve and connect to that host:port from a client of the emulator — the '.rds.localhost' authority is not one the emulator serves.
4. Replay logs__rds.obs-rds-3-aurora-postgresql-log-export-error-metric-filter-counts-only-error-lines.
