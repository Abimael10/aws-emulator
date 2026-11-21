Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances
Issue-Key: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances|run#13|AssertionError|assert reader_host and reader_host != writer_host, (
Issue-Commit: 563d9546b2aa96bbba80dbdb1955fbc3f9af54b6

# rds__secretsmanager: a zero-member Aurora cluster's ReaderEndpoint is now IDENTICAL to its Endpoint, so the cluster-minted read path is indistinguishable from the write path

> _Note: this issue was identified against the code base at commit `563d9546b2aa96bbba80dbdb1955fbc3f9af54b6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
One root cause. Cause count: 1.

=== Cause 1 of 1: DescribeDBClusters returns the same value for Endpoint and ReaderEndpoint on a cluster with zero members ===
AWS: Smithy (com.amazonaws.rds#AmazonRDSv19) DBCluster carries `Endpoint` (the writer connection endpoint), `ReaderEndpoint` — documented as "The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster" — `Port` (an Integer) and `HostedZoneId`. Both endpoint members are DNS host NAMES only, with the port carried separately in Port. Aurora mints them from the cluster identifier over one shared DNS suffix: `<DBClusterIdentifier>.cluster-<suffix>` for the writer and `<DBClusterIdentifier>.cluster-ro-<the same suffix>` for the reader. The reader endpoint is minted by the CLUSTER, not by any reader instance, so it exists, is non-empty and keeps that form even when DBClusterMembers and ReadReplicaIdentifiers are both empty. The identifier-derived prefix, the discriminating `cluster-`/`cluster-ro-` infix, and the two values being DISTINCT are the requirement; the concrete suffix, zone, address and port of one environment are not.
Emulator: CreateDBCluster(Engine='aurora-postgresql', ServerlessV2ScalingConfiguration, no instances) followed by DescribeDBClusters returns the cardinality correctly (DBClusterMembers == [], ReadReplicaIdentifiers == []) but publishes `Endpoint` and `ReaderEndpoint` as the identical bare address — the replay observed both as '127.0.0.1' (the literal address is environment-specific and is not the finding). No identifier prefix, no `cluster-ro-` infix, no shared DNS suffix, and now not even two different strings.
Why: a client cannot tell the reader endpoint from the writer endpoint — the only discriminator is gone and the equality check that a read/write-splitting client, a DSN builder or a production-readiness audit uses to prove a read path exists yields "no read path". Reads and writes both land on the write target, and nothing composed from Endpoint/ReaderEndpoint plus Port/HostedZoneId is a valid Aurora connection target.
Fix: the RDS cluster-endpoint representation in the rds crate — where CreateDBCluster / RestoreDBClusterFromSnapshot mint a cluster's endpoints into state and where DescribeDBClusters serializes DBCluster.Endpoint / ReaderEndpoint / Port / HostedZoneId. The served address must stay reachable, but the two endpoint names must be distinct, cluster-identifier-derived with the `cluster-` and `cluster-ro-` infixes over one shared suffix, regardless of member count, with the port only in Port.
Related (does NOT cover this failure): open issues 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md and 20261116T052754Z-rds-secretsmanager-the-aurora-production-readiness-audit-cannot-derive-read.md describe the same representation defect, but both are keyed on the earlier state where ReaderEndpoint was a DISTINCT 'host:port' literal ('127.0.0.1:33453') and the check failed on the later `startswith('<id>.cluster-ro-')` assertion. The emulator now fails an EARLIER assertion — the two endpoints are equal — so this failure is not the one those keys carry and the suite did not mark it owned.
Traces: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances (mismatch).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check [mismatch] rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances:

  File "check_rds__secretsmanager_refarch_aur_reader_endpoint_exists_with_zero_reader_instances.py", line 57, in run
    assert reader_host and reader_host != writer_host, (
  AssertionError: the cluster must expose a reader endpoint distinct from the writer endpoint even with 0 readers; writer='127.0.0.1' reader='127.0.0.1'

Expected (AWS): DescribeDBClusters.DBClusters[0].ReaderEndpoint is a non-empty bare DNS host '<DBClusterIdentifier>.cluster-ro-<shared-suffix>', DISTINCT from Endpoint '<DBClusterIdentifier>.cluster-<shared-suffix>', both under the same non-empty suffix, with the port only in the Integer member Port and the zone in HostedZoneId — even though DBClusterMembers is empty.

Actual (emulator): HTTP 200; DBClusterMembers == [] and ReadReplicaIdentifiers == [] (both correct), but Endpoint == ReaderEndpoint == the same bare address (observed '127.0.0.1' for both). The literal address is environment-specific and is not the requirement; the divergence is that the two members are equal and carry no cluster-identifier-derived 'cluster-'/'cluster-ro-' structure. The later assertions on the 'cluster-ro-' prefix and the shared suffix are never reached.

## Steps to Reproduce
1. Boot the emulator and point an RDS client at it.
2. CreateDBSubnetGroup over the run's shared network subnets.
3. CreateDBCluster(DBClusterIdentifier="<id>", Engine="aurora-postgresql", EngineVersion=<newest from DescribeDBEngineVersions>, MasterUsername="dbadmin", MasterUserPassword=..., ServerlessV2ScalingConfiguration={MinCapacity:0.5, MaxCapacity:1.0}) and create NO DB instances.
4. Poll DescribeDBClusters(DBClusterIdentifier="<id>") until Status == "available".
5. Read DBClusters[0].Endpoint and DBClusters[0].ReaderEndpoint.
Expected: two different non-empty host-only names, "<id>.cluster-<suffix>" and "<id>.cluster-ro-<same suffix>", port only in Port, zone in HostedZoneId.
Actual: both members hold the identical bare address, so there is no distinguishable reader endpoint at all.
