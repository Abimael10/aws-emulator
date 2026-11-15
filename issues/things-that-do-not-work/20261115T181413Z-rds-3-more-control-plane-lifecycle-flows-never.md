Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.read-replica-promote-lifecycle rds.create-shard-group-wrong-cluster-state-rejected rds.delete-cluster-conflicting-snapshot-name-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.read-replica-promote-lifecycle|_wait_until#12|Timeout|last = probe()
Issue-Key: rds.create-shard-group-wrong-cluster-state-rejected|_wait_cluster_status#15|Timeout|time.sleep(poll_s)
Issue-Key: rds.delete-cluster-conflicting-snapshot-name-rejected|_wait_cluster_gone#27|Timeout|time.sleep(poll_s)
Issue-Commit: 860eb666e989875e1daa85fefb4b7f7b73b30cd3

# rds: 3 more control-plane lifecycle flows never converge, so the checks polling them are killed at the 60 s budget (oracle-ee replica switchover/promote, Limitless cluster stop→start + shard group, Aurora cluster + cluster-snapshot availability)

> _Note: this issue was identified against the code base at commit `860eb666e989875e1daa85fefb4b7f7b73b30cd3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
Three lifecycle flows of the RDS emulator never reach the terminal state their operations promise, so every check that polls them is killed by the suite's 60 s per-check budget and reaches no verdict. Each cause is numbered and lists its own failing check id; a fixer must close all three before submitting. These are NET-NEW flows: the open issues that already carry convergence failures — 20261110T191704Z-rds-six-control-plane-lifecycle-flows-never-reach.md, 20261111T084329Z-rds-ten-more-control-plane-and-data-plane.md and 20261112T074631Z-rds-snapshot-copy-restore-read-replica-flows-never.md — do not carry these three checks' replay keys.

=== Cause 1 of 3: the oracle-ee read-replica switchover/promote lifecycle never converges ===
AWS: CreateDBInstance(Engine=oracle-ee) reaches `available`; CreateDBInstanceReadReplica from it reaches `available` with no PendingModifiedValues, reports `ReadReplicaSourceDBInstanceIdentifier` = the source and `StatusInfos[0].Status` = `replicating`; ModifyDBInstance(BackupRetentionPeriod=1, ApplyImmediately=true) settles back to `available` with the new retention applied and PendingModifiedValues cleared; SwitchoverReadReplica (Smithy: errors DBInstanceNotFoundFault 404, InvalidDBInstanceStateFault 400) exchanges the roles so the former standby carries no source identifier and the former primary reports the standby as its source, both settling to `available`; PromoteReadReplica then leaves the promoted instance `available` with no source identifier.
Emulator: the check is killed at the 60 s budget with every operation answered but no polled predicate ever satisfied. Hypothesis: one of the states on this path is never settled — the replica never reports `StatusInfos[0].Status == 'replicating'`, or PendingModifiedValues is never cleared after the ApplyImmediately=true modify, or the `modifying` → `available` settle the switchover schedules for BOTH instances is never made due — so the poll loop never advances.
Why: a client that waits on these documented terminal states (as every SDK waiter and every Data Guard runbook does) waits forever; the whole replica role-transition surface is unusable even though each call answers 200.
Fix: the instance lifecycle settle in services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs (the due-transition settle and PendingStatus/PendingModifiedValues bookkeeping) together with src/ops/switchover_read_replica.rs and src/ops/promote_read_replica.rs, which must leave both instances settling to `available` and must publish the replica's StatusInfos.
Traces: rds.read-replica-promote-lifecycle.

=== Cause 2 of 3: a Limitless Aurora PostgreSQL cluster never converges through available → stopped → available, so the CreateDBShardGroup state refusal is never reached ===
AWS: CreateDBCluster(Engine=aurora-postgresql, ClusterScalabilityType=limitless, StorageType=aurora-iopt1) reaches Status `available`; StopDBCluster settles it to `stopped`; CreateDBShardGroup on a stopped cluster is then refused with `InvalidDBClusterStateFault` and the message "You can't create the DB shard group. The DB cluster <DBClusterIdentifier> isn't in the AVAILABLE or CREATING state." (fixed text with the request's own cluster identifier as the only variable part), creating nothing — DescribeDBShardGroups for the named group answers `DBShardGroupNotFound`; StartDBCluster returns the cluster to `available` and the IDENTICAL CreateDBShardGroup then succeeds and the group reaches `available`.
Emulator: the check is killed at the 60 s budget while polling DescribeDBClusters for one of those statuses, so neither the refusal nor the positive control is ever observed. Hypothesis: the cluster is parked in a transient status (`creating`, or `stopping` after StopDBCluster) whose settle is never made due.
Why: a client cannot stop or start a Limitless cluster and cannot rely on the shard-group state precondition; any workflow gated on those statuses hangs.
Fix: the cluster lifecycle settle in src/state.rs plus src/ops/stop_db_cluster.rs / src/ops/start_db_cluster.rs (and the shard-group status settle in src/ops/create_db_shard_group.rs), so a stop and a start reach their documented terminal statuses.
Traces: rds.create-shard-group-wrong-cluster-state-rejected.

=== Cause 3 of 3: an Aurora cluster plus its manual cluster snapshot never both reach `available`, so DeleteDBCluster's conflicting-final-snapshot refusal is never reached ===
AWS: CreateDBCluster(Engine=aurora-postgresql, ServerlessV2ScalingConfiguration) reaches Status `available`; CreateDBClusterSnapshot reaches Status `available`; DeleteDBCluster(SkipFinalSnapshot=false, FinalDBSnapshotIdentifier=<that existing snapshot>) is then refused with `DBClusterSnapshotAlreadyExistsFault` and the message "Cannot create the cluster snapshot because one with the identifier <FinalDBSnapshotIdentifier> already exists." (fixed text; the identifier is the one the request supplied), leaving the cluster and the snapshot untouched (the snapshot's SnapshotCreateTime is unchanged); deleting with a genuinely new final-snapshot name succeeds, the cluster disappears and the new snapshot reaches `available`.
Emulator: the check is killed at the 60 s budget while polling for one of those `available` statuses, so the refusal and its "effect absent" assertions are never reached. Hypothesis: the cluster or the cluster snapshot is parked in `creating` and its settle is never made due.
Why: a client cannot wait on a cluster or a cluster snapshot becoming available, which gates every snapshot and delete workflow.
Fix: the cluster and cluster-snapshot status settle in src/state.rs together with src/ops/create_db_cluster.rs and src/ops/create_db_cluster_snapshot.rs.
Traces: rds.delete-cluster-conflicting-snapshot-name-rejected.

## Evidence
All three failures are reported by the suite as "Error: Check timed out after 60.0s" with no assertion reached:
- rds.read-replica-promote-lifecycle — polls DescribeDBInstances for: oracle-ee primary `available`; replica `available` with no PendingModifiedValues; replica BackupRetentionPeriod > 0 after ModifyDBInstance(ApplyImmediately=true); after SwitchoverReadReplica, the former standby `available` with NO ReadReplicaSourceDBInstanceIdentifier and the former primary `available` with ReadReplicaSourceDBInstanceIdentifier = the new primary; after PromoteReadReplica, `available` with no source identifier. Expected: each predicate satisfied. Actual: none reached inside the budget.
- rds.create-shard-group-wrong-cluster-state-rejected — polls DescribeDBClusters for `available`, then `stopped` after StopDBCluster, then `available` after StartDBCluster, before asserting InvalidDBClusterStateFault "You can't create the DB shard group. The DB cluster <id> isn't in the AVAILABLE or CREATING state." and DBShardGroupNotFound. Expected: those statuses. Actual: timed out polling.
- rds.delete-cluster-conflicting-snapshot-name-rejected — polls DescribeDBClusters for `available` and DescribeDBClusterSnapshots for `available` before asserting DBClusterSnapshotAlreadyExistsFault "Cannot create the cluster snapshot because one with the identifier <id> already exists." Expected: those statuses. Actual: timed out polling.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite (run_behavioral_suite); the three checks in trace_ids end as "Check timed out after 60.0s".
2. Cause 1: CreateDBInstance(Engine=oracle-ee, LicenseModel=bring-your-own-license) and poll DescribeDBInstances for `available`; CreateDBInstanceReadReplica(ReplicaMode=mounted) and poll for `available` with no PendingModifiedValues and StatusInfos[0].Status == 'replicating'; ModifyDBInstance(BackupRetentionPeriod=1, ApplyImmediately=true) and poll for it applied; SwitchoverReadReplica then PromoteReadReplica and poll the role/source changes — note which predicate never becomes true.
3. Cause 2: CreateDBCluster(Engine=aurora-postgresql, ClusterScalabilityType=limitless, StorageType=aurora-iopt1, MonitoringRoleArn=…) and poll DescribeDBClusters for `available`; StopDBCluster and poll for `stopped`; CreateDBShardGroup and check the refusal; StartDBCluster and poll for `available`.
4. Cause 3: CreateDBCluster(Engine=aurora-postgresql, ServerlessV2ScalingConfiguration) and poll for `available`; CreateDBClusterSnapshot and poll DescribeDBClusterSnapshots for `available`; DeleteDBCluster(SkipFinalSnapshot=false, FinalDBSnapshotIdentifier=<that snapshot>) and check the refusal.
5. Re-replay the checks in trace_ids to confirm the fix.
