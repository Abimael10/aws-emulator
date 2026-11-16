Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle
Issue-Key: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: c817d132dac12b2959b1490cc2289f359c16fb32

# The Aurora production-readiness audit cannot derive read_path_isolation: DescribeDBClusters ReaderEndpoint is a literal host:port with no cluster-ro- infix

> _Note: this issue was identified against the code base at commit `c817d132dac12b2959b1490cc2289f359c16fb32`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
One root cause. Cause count: 1.

=== Cause 1 of 1: a DB cluster's reader endpoint is not a cluster-identifier-derived DNS name, so an audit that reads it cannot distinguish the read path from the write path ===

AWS: Smithy (com-amazonaws-rds-amazonrdsv19) DBCluster models how a cluster is reached with separate members: Endpoint (the writer connection endpoint), ReaderEndpoint — documented as "The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster..." — Port (an Integer, the one port both endpoints are reached on) and HostedZoneId (the Route 53 zone the endpoint names live in). The endpoint members carry a DNS host NAME only and never an embedded port, because the port is modelled separately. Aurora mints both names from the cluster identifier: the writer as <DBClusterIdentifier>.cluster-<shared-suffix> and the reader as <DBClusterIdentifier>.cluster-ro-<shared-suffix>, over one DNS suffix (the regional zone named by HostedZoneId). The identifier-derived prefix and the discriminating cluster- vs cluster-ro- infix are the requirement; the concrete suffix, zone, address and port of any one environment are not.

Emulator: DescribeDBClusters on a CloudFormation-deployed Aurora Serverless v2 cluster returns a ReaderEndpoint that is a literal socket address with the port glued on (the replay observed '127.0.0.1:34223'). It carries no <DBClusterIdentifier> prefix, no cluster-ro- infix, and is not a bare host name. The concrete address is environment-specific and is not the finding; the finding is the missing <identifier>.cluster-ro-<suffix> structure and the port embedded in the host string.

Why: a client cannot tell the reader endpoint from the writer endpoint, because the only discriminator (cluster- vs cluster-ro-) is gone, and every consumer that composes a connection target from Endpoint/ReaderEndpoint plus Port (drivers, DSN/JDBC URL builders, code that splits the DNS suffix or pairs the host with HostedZoneId) produces a malformed target with the port twice. Read/write splitting silently sends both to the same place. Concretely here, a four-phase production-readiness audit that derives read_path_isolation from the cluster's endpoints fails in its solve phase and the remaining audit findings and the cleanup/destroy phases are never reached.

Fix: the RDS cluster-endpoint representation — where CreateDBCluster / RestoreDBClusterFromSnapshot mint a cluster's endpoints into state and where DescribeDBClusters serializes DBCluster.Endpoint / ReaderEndpoint / Port / HostedZoneId. The address must stay reachable, but the port belongs in Port and both endpoint names must be derived from the cluster identifier with the cluster- and cluster-ro- infixes over one shared DNS suffix, regardless of how many members the cluster has. Open issue 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md describes the same representation defect but carries only the replay key of rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances; this check is a second, unowned grounding of it (a CFN-deployed cluster inside an e2e lifecycle rather than a bare zero-member cluster) and must be re-replayed too.

Traces: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle (mismatch), phase 2/4 'solve'.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check [mismatch] rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle:

  File "check_rds__secretsmanager_refarch_aur_e2e_review_aurora_production_readiness_lifecycle.py", line 231, in phase
    raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
  AssertionError: [phase 2/4 solve] the reader endpoint must carry the cluster-ro- infix, got '127.0.0.1:34223'

Expected (AWS): DescribeDBClusters.DBClusters[0].ReaderEndpoint is a bare DNS host of the form <DBClusterIdentifier>.cluster-ro-<shared-suffix>, distinct from Endpoint (<DBClusterIdentifier>.cluster-<shared-suffix>), both under the same non-empty DNS suffix, with the port reported only in the Integer member Port and the zone in HostedZoneId.

Actual (emulator): ReaderEndpoint = "<address>:<port>" (observed '127.0.0.1:34223') — no identifier-derived prefix, no cluster-ro- infix, port embedded in the host string. The literal address and port shown are environment-specific and are not the requirement; the missing structure is. The trajectory steps before it (rds.DescribeDBClusters fired, cluster reported available) succeeded, so only the endpoint representation diverges, and the audit's read_path_isolation finding plus the cleanup and destroy phases are never reached.

## Steps to Reproduce
1. Boot the emulator and point rds/cloudformation/secretsmanager clients at it.
2. CreateStack with the cfn-aurora-serverless template (an aurora DB cluster with deletion protection on, a DB subnet group, and a Secrets Manager secret for the master credentials); wait for CREATE_COMPLETE.
3. Call rds.DescribeDBClusters(DBClusterIdentifier=<the stack's cluster id>) and read DBClusters[0].
4. Inspect ReaderEndpoint: it is a "host:port" literal rather than a name beginning "<cluster-id>.cluster-ro-", and Endpoint likewise does not begin "<cluster-id>.cluster-"; the two share no DNS suffix after their per-cluster prefix, and Port duplicates the port already embedded in the host string.
Expected: ReaderEndpoint == "<cluster-id>.cluster-ro-<suffix>", Endpoint == "<cluster-id>.cluster-<suffix>", same <suffix>, port only in Port, zone in HostedZoneId.
