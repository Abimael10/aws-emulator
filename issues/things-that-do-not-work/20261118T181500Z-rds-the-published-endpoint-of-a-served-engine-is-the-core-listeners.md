Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.aurora-writer-reader-endpoint-query-routing rds.deep-rds-sql-truth-force-ssl-plaintext-login-rejected rds.snaprestore-final-snapshot-on-delete-restores-rows rds.rds-sql-connectivity-postgres-mysql rds.create-shard-group-wrong-cluster-state-rejected rds.read-replica-promote-lifecycle rds.read-replica-replication-query-and-promote rds.replica-uncommitted-primary-write-absent-until-commit rds.deploy-rds-2-cfn-mysql-sql-then-dbname-change-replaces-instance
Issue-Key: rds.aurora-writer-reader-endpoint-query-routing|run#30|AssertionError|assert port == 5432, f"aurora-postgresql must listen on 5432, got {port!r}"
Issue-Key: rds.aurora-writer-reader-endpoint-query-routing|run#23|AssertionError|assert writer_ep != reader_ep, (
Issue-Key: rds.deep-rds-sql-truth-force-ssl-plaintext-login-rejected|run#21|AssertionError|assert port == _PORT, f"postgres must listen on {_PORT}; DescribeDBInstances says {port!r}"
Issue-Key: rds.snaprestore-final-snapshot-on-delete-restores-rows|run#29|AssertionError|assert dst_addr != src_addr, (
Issue-Key: rds.rds-sql-connectivity-postgres-mysql|_sql_roundtrip_with_retry#12|Timeout|raise AssertionError(
Issue-Key: rds.create-shard-group-wrong-cluster-state-rejected|_error#18|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: rds.create-shard-group-wrong-cluster-state-rejected|_wait_cluster_status#15|Timeout|time.sleep(poll_s)
Issue-Key: rds.read-replica-promote-lifecycle|run#17|KeyError|assert replica_before["StatusInfos"][0]["Status"] == "replicating", (
Issue-Key: rds.read-replica-promote-lifecycle|_wait_until#12|Timeout|last = probe()
Issue-Key: rds.read-replica-replication-query-and-promote|run#24|AssertionError|assert rep_addr != src_addr, (
Issue-Key: rds.replica-uncommitted-primary-write-absent-until-commit|run#14|AssertionError|assert rep_addr != src_addr, (
Issue-Key: rds.deploy-rds-2-cfn-mysql-sql-then-dbname-change-replaces-instance|run#55|AssertionError|assert host_b != host_a, f"replacement kept the same Endpoint.Address {host_a!r}"
Issue-Commit: 8b0f35809934c567f1bf7b94b8ce8845151fefb8

# rds: the endpoint a served engine publishes cannot be both AWS's (engine port, per-resource DNS address) and dialable, and three checks die in a wait this fixer could not place

> _Split out of `issues/things-that-now-work/20261108T051751Z-rds-7-net-new-causes-endpoint-port-is.md`, whose cause 4 (retained automated backups) is fixed and whose causes 2, 3 and 5 were already implemented on mainline by the time it was worked. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
DescribeDBInstances

## Description
Two kinds of open cause. The first is BLOCKED on the read-only core and is the reason four
checks diverge: the address and port a resource whose engine wire this emulator SERVES
publishes are the core listener's, not AWS's. The second is three checks that die inside a
wait of their own; the behaviour each of them was filed for is implemented and tested in
the crate, so what is missing is the step before it, which this fixer could not place from
the replay output. What the parent fix DID land, so a later fixer does not redo it: the
retained automated backups of a deleted DB instance
(`crate::state::RetainedInstanceBackupRecord`, `DescribeDBInstanceAutomatedBackups`,
`DeleteDBInstanceAutomatedBackup`, `tests/all/db_instance_automated_backups.rs`), and a
test that pins the postgres+mysql+mariadb provisioning flow end to end over real sockets
(`tests/all/db_instance_sql_connectivity.rs`).

=== Cause 1 of 4: a served engine's `Endpoint` publishes the core listener's host and port, so it is neither the engine's port nor per-resource ===
AWS: `Endpoint$Port` is "the port that the database engine is listening on" and
`CreateDBInstanceMessage$Port` defaults per engine family (PostgreSQL 5432, MySQL/MariaDB
3306, Oracle 1521, SQL Server 1433, Db2 50000) or echoes the requested value;
`DBInstance$Endpoint`'s `Address` is "the DNS address of the DB instance", one name per
instance derived from its own identifier, so two instances never share one; and a DB
cluster publishes a WRITER endpoint and a `ReaderEndpoint` that are different hostnames.
Emulator: for every engine whose wire this emulator serves (postgres, mysql, mariadb, and
an Aurora cluster's volume), `Endpoint.Address` is the core's bind interface — `127.0.0.1`
for every such resource in the process — and `Endpoint.Port` is the port the OS assigned
that resource's listener (observed 40315 for a postgres instance, 34189 for a mysql one),
so a restored instance publishes the same address as the instance it was restored from and
a cluster's writer and reader endpoints are the same hostname. An engine this emulator
serves NO wire for already publishes AWS's form (an `oracle-ee` instance publishes
`<identifier>.<host>.<region>.rds.amazonaws.com` on 1521), which is what makes this a
property of the served path alone.
Why: a client that reads the endpoint cannot tell two instances apart, cannot verify a
restore, and cannot split reads from writes on a cluster; "postgres listens on 5432" is
false here.
Blocked by: `vendor/server/src/listeners.rs` — `bind_std`/`bind_tokio` (lines 67-107) bind
every resource listener on the ONE operator-chosen interface (loopback by default) at an
OS-assigned port, and `ResourceListener::host()`/`port()` (lines 718-723) are the only
address a service can publish. A served resource MUST publish the address a client dials
(the project's data-plane guidance states exactly that, and announcing an endpoint nothing
protocol-capable serves is a blocking review finding), so the two requirements cannot both
hold from inside a service crate. REQUIRED CORE CHANGE: let a service open a listener at a
requested port and on a per-resource loopback alias (or under a per-resource name the host
resolves), so each resource can be dialled at its own address on its engine's own port;
then the control plane can publish AWS's form and still be reachable.
Traces: rds.aurora-writer-reader-endpoint-query-routing (both endpoints `127.0.0.1`, and
the port assertion behind it), rds.deep-rds-sql-truth-force-ssl-plaintext-login-rejected
(5432 expected), rds.snaprestore-final-snapshot-on-delete-restores-rows (the restored
instance's address equals the source's), rds.read-replica-replication-query-and-promote and
rds.replica-uncommitted-primary-write-absent-until-commit (a read replica advertises
`127.0.0.1`, the same host as the source it was made from, so a reader cannot be pointed at
it), rds.deploy-rds-2-cfn-mysql-sql-then-dbname-change-replaces-instance (the instance
CloudFormation provisions for a `DBName` replacement publishes the replaced instance's
address).

> _Those last three keys were re-filed here from
> `issues/things-that-now-work/20261115T181413Z-rds-3-net-new-behavioral-causes-every-db.md`,
> whose causes 2 and 3 are fixed and whose cause 1 is this same cause: two live DB instances
> of a SERVED engine share the one address the core binds. They are kept on this report so
> they are not recorded as working while the core change above is outstanding._

=== Cause 2 of 4: the Limitless shard-group check never gets past its cluster wait ===
AWS: `CreateDBShardGroup` against a cluster that is not available is refused
`InvalidDBClusterStateFault`, and the identical call succeeds once that cluster is started.
Emulator: the refusal IS implemented and tested — `src/ops/create_db_shard_group.rs` refuses
a cluster that cannot take a shard group ("You can't create the DB shard group. The DB
cluster <id> isn't in the AVAILABLE or CREATING state."), stores nothing, and
`tests/all/db_shard_group_cluster_state.rs` tables it over creating/available/stopping/
stopped and serves the same call after `StartDBCluster`. The check nevertheless dies inside
`_wait_cluster_status` before the shard-group call is made. Ruled out by probe over the
wire: `CreateDBCluster` with `ClusterScalabilityType=limitless`,
`EngineVersion=16.4-limitless`, `StorageType=aurora-iopt1`, `EnablePerformanceInsights`,
`PerformanceInsightsRetentionPeriod=31`, `MonitoringInterval=30`, a `MonitoringRoleArn` and
`EnableCloudwatchLogsExports=[postgresql]` is ACCEPTED and settles to `available`, and
`StopDBCluster`/`StartDBCluster` settle to `stopped`/`available`.
Still to find: WHICH value the check's wait polls for. Two candidates a later fixer should
start from, both modelled and both absent here: `DBCluster$LimitlessDatabase` (a
`LimitlessDatabase` with a `Status` from `active | not-in-use | enabled | disabled |
enabling | disabling | modifying-max-capacity | error` and a `MinRequiredACU`) is reported
for NO cluster, including one created with `ClusterScalabilityType=limitless`; and the
engine-version catalogue publishes exactly one Limitless build, `16.4-limitless`, mapped to
the `aurora-postgresql16` parameter-group family, so a create naming another Limitless
build is refused "Cannot find version <v> for aurora-postgresql" and
`CreateDBClusterParameterGroup` with the family AWS documents for Limitless
(`aurora-postgresql16-limitless`) is refused "is not a valid parameter group family".
Traces: rds.create-shard-group-wrong-cluster-state-rejected.

=== Cause 3 of 4: the read-replica promotion check never gets past its own wait ===
AWS: a replica reports `StatusInfos` with one entry whose `StatusType` is "read
replication" and whose `Status` is "replicating", and `PromoteReadReplica` clears it.
Emulator: that IS implemented — `src/state/instances.rs::status_infos` reports exactly that
entry for an instance that replicates another and none for one that does not,
`tests/all/read_replica_replication_and_promotion.rs` asserts it, and a probe over the wire
(`CreateDBInstance(Engine=oracle-ee, BackupRetentionPeriod=1)` then
`CreateDBInstanceReadReplica`) shows `<StatusInfos><DBInstanceStatusInfo><StatusType>read
replication</StatusType><Normal>true</Normal><Status>replicating</Status>` on the replica.
The check nevertheless dies inside `_wait_until` before the assertion that was filed.
Still to find: which condition that wait polls for (the source's
`ReadReplicaDBInstanceIdentifiers`, a status the promotion passes through, or a member of
the promoted instance), and which member of the flow never reaches it.
Traces: rds.read-replica-promote-lifecycle.

=== Cause 4 of 4: the postgres+mysql SQL round trip never completes in the replay ===
AWS: a `db.t3.micro` postgres instance and a `db.t3.micro` mysql instance each reach
`available`, publish an `Endpoint` that accepts a native session and round-trip a row.
Emulator: the flow completes in-process — `tests/all/db_instance_sql_connectivity.rs`
provisions a postgres, a mysql and a mariadb instance side by side in ONE region, polls each
to `available`, opens a session on each published `Endpoint` over a real TCP connection (one
in the clear, two upgraded in band to TLS; one with a `DBName`, one on the engine's default
database) and round-trips a row, in under two seconds. The replayed check still spends its
whole budget in `_sql_roundtrip_with_retry`.
Hypothesis (not a finding): every replayed unit that needs a WORKING native session fails
this way, while the unit that expects a session to be REFUSED passes, which is what a
client that cannot reach the core's loopback listener would produce. A later fixer should
establish first whether the replay client can dial the address the control plane publishes
at all; if it can, the defect is in the session this emulator serves and the test above is
the place to extend.
Traces: rds.rds-sql-connectivity-postgres-mysql.

## Evidence
Cause 1: AssertionError: Endpoint and ReaderEndpoint must be different hostnames — every
read-scaling client relies on the split; both are '127.0.0.1'. AssertionError: the restored
instance must have its own endpoint address, not the destroyed source's ('127.0.0.1').
AssertionError: aurora-postgresql must listen on 5432, got <an OS-assigned port>.
Cause 2: Error: Check timed out after 60.0s, inside `_wait_cluster_status`.
Cause 3: Error: Check timed out after 60.0s, inside `_wait_until`.
Cause 4: Error: Check timed out after 60.0s, inside `_sql_roundtrip_with_retry`.

## Steps to Reproduce
Cause 1: `CreateDBInstance(Engine=postgres)` and a second one, poll both to `available` and
compare `Endpoint.Address` and `Endpoint.Port` — the addresses must differ and each port
must be the engine's (5432, or the requested `Port`). Then `CreateDBCluster(Engine=aurora-postgresql)`
with a writer member and compare `Endpoint` with `ReaderEndpoint`.
Cause 2: create a Limitless-capable aurora-postgresql cluster the way the check does, poll
the status it waits for, `StopDBCluster`, then `CreateDBShardGroup` — the refusal itself is
already served.
Cause 3: `CreateDBInstance` primary, `CreateDBInstanceReadReplica`, poll the condition the
check waits for, then `PromoteReadReplica`.
Cause 4: `CreateDBInstance(Engine=postgres)` and `CreateDBInstance(Engine=mysql)`, poll both
to `available`, and open a native session to each published endpoint from the replay client.
