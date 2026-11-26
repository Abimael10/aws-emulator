Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances
Issue-Key: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances|run#13|AssertionError|assert reader_host and reader_host != writer_host, (
Issue-Commit: bf5aedd86bc1c51d8b010649bf7e2337ac48d430

# rds__secretsmanager: a served Aurora DB cluster publishes the SAME host for Endpoint and ReaderEndpoint, so the reader endpoint is indistinguishable from the writer

> _Note: this issue was identified against the code base at commit `bf5aedd86bc1c51d8b010649bf7e2337ac48d430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
Cause 1 of 1: for a DB cluster whose data plane this emulator serves, DescribeDBClusters publishes the SAME string in DBCluster.Endpoint and DBCluster.ReaderEndpoint — the served host — so the response carries nothing that distinguishes a reader endpoint from a writer endpoint, and nothing derived from the cluster identifier.

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) DBCluster carries Endpoint ("the connection endpoint for the primary instance of the DB cluster"), ReaderEndpoint ("The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster"), one integer Port both endpoints are reached on, and HostedZoneId. Aurora mints the two names from the cluster identifier over one shared DNS suffix: the writer as '<DBClusterIdentifier>.cluster-<suffix>' and the reader as '<DBClusterIdentifier>.cluster-ro-<suffix>'. The requirement is structural and holds for a cluster with ZERO members as well (the reader name is minted by the cluster, not by any reader instance): the identifier-derived prefix, the discriminating 'cluster-' vs 'cluster-ro-' infix, a non-empty suffix shared by both names, a non-empty HostedZoneId and the port reported only in Port. The concrete suffix, zone id and port of any one environment are per-run values and are NOT part of the requirement — an emulator may keep the address reachable as long as the two published names differ in that infix.

Emulator: CreateDBCluster succeeds and the cluster settles 'available' with DBClusterMembers == [] and ReaderEndpoint == Endpoint == the emulator's own served host (observed as the loopback literal '127.0.0.1' for both members in the same response). services/service-com-amazonaws-rds-amazonrdsv19/src/state/clusters.rs makes this explicit: DbClusterRecord::reader_endpoint() returns self.serves_at_its_own_host() first and only falls back to minted_endpoint_name(CLUSTER_READER_LABEL) for a cluster this emulator serves no plane for — and DbClusterRecord::endpoint() does the same with CLUSTER_WRITER_LABEL, so both members collapse onto one identical host whenever a plane IS served. The projection in the same file sets endpoint: Some(self.endpoint()) and reader_endpoint: Some(self.reader_endpoint()) from those two readings.

Why: the 'cluster-' vs 'cluster-ro-' infix is the ONLY thing in the response that tells the two endpoints apart. A client, DSN builder, connection-pool configuration or reviewer that routes writes to Endpoint and reads to ReaderEndpoint cannot tell that it is sending both to the same place; anything that derives or validates the reader name from the cluster identifier, pairs it with HostedZoneId, or splits its DNS suffix sees a value with no structure at all. Code that asserts 'reader != writer' — the documented shape of an Aurora cluster, even with no Aurora Replica — is wrong against this emulator.

Fix: the DB-cluster endpoint representation in the rds crate's cluster state and the DescribeDBClusters/CreateDBCluster projection — services/service-com-amazonaws-rds-amazonrdsv19/src/state/clusters.rs (DbClusterRecord::endpoint / ::reader_endpoint / ::serves_at_its_own_host and the cluster_to_wire projection) and src/state/cluster_endpoint_names.rs, which owns the minted names and their shared suffix. The served-plane shortcut must not collapse the two members onto one host: both names have to keep the identifier-derived 'cluster-' / 'cluster-ro-' form over one shared suffix (while remaining reachable from a client of the emulator), and the port must be reported only in Port.

Related open issue that does NOT carry this check's replay key: 20261125T130223Z-rds-secretsmanager-a-cloudformation-deployed-aurora-cluster-publishes.md describes the same missing structure on a CloudFormation-deployed Aurora cluster (ReaderEndpoint == 'localhost'), and it in turn names 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md. This check reaches the field on a DIRECTLY created, served Aurora Serverless v2 cluster with zero members and observes writer == reader == the served loopback host; the replay report did not mark it OWNED by either. A fixer closing them together must make every creation path (direct create, CloudFormation provider, served and unserved planes) publish the identifier-derived pair.

Traces: rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances.

## Evidence
Check rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances [mismatch]. Everything before the endpoint comparison passed — CreateDBCluster (aurora-postgresql, Serverless v2, newest 16.x engine version) returned 200, the cluster polled to Status 'available', DBClusterMembers == [] and ReadReplicaIdentifiers == [] — so the endpoint representation is the sole divergence reached:

  File "check_rds__secretsmanager_refarch_aur_reader_endpoint_exists_with_zero_reader_instances.py", line 57, in run
    assert reader_host and reader_host != writer_host, (
  AssertionError: the cluster must expose a reader endpoint distinct from the writer endpoint even with 0 readers; writer='127.0.0.1' reader='127.0.0.1'

Expected (AWS): DescribeDBClusters.DBClusters[0].ReaderEndpoint is a DNS host name beginning '<DBClusterIdentifier>.cluster-ro-', Endpoint begins '<DBClusterIdentifier>.cluster-' and does NOT begin '<DBClusterIdentifier>.cluster-ro-', the two share one non-empty DNS suffix, HostedZoneId is a non-empty string and Port is a positive integer carrying the port for both.
Actual (emulator): ReaderEndpoint == Endpoint, both the bare served host. '127.0.0.1' and the generated cluster identifier are per-run values filling the template; the requirement is that the two members differ in the 'cluster-' / 'cluster-ro-' infix and are derived from the cluster identifier over a shared suffix.

Source of the collapse (cited for the fixer, not as a recorded value): services/service-com-amazonaws-rds-amazonrdsv19/src/state/clusters.rs lines ~585 and ~692 — endpoint() and reader_endpoint() both return serves_at_its_own_host() when a data plane is served, and only otherwise mint distinct CLUSTER_WRITER_LABEL / CLUSTER_READER_LABEL names; the projection at lines ~976-977 writes both into the wire shape.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. Create a DB subnet group over the run's network, then rds.create_db_cluster(Engine='aurora-postgresql', EngineVersion=<newest 16.x from DescribeDBEngineVersions>, DBClusterIdentifier=<id>, MasterUsername/MasterUserPassword set, ServerlessV2ScalingConfiguration={'MinCapacity':0.5,'MaxCapacity':1.0}) and create NO instances.
3. Poll rds.describe_db_clusters(DBClusterIdentifier=<id>) until Status == 'available'; confirm DBClusterMembers == [] and ReadReplicaIdentifiers == [].
4. Read DBClusters[0].Endpoint, .ReaderEndpoint, .HostedZoneId and .Port.
5. Observe Endpoint == ReaderEndpoint (both the bare served host, e.g. the loopback literal) — no '<id>.cluster-' / '<id>.cluster-ro-' prefixes, no shared DNS suffix — where AWS publishes two names differing only in the 'cluster-ro-' infix.
6. Replay rds__secretsmanager.refarch-aur-reader-endpoint-exists-with-zero-reader-instances to confirm a fix.
