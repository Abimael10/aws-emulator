Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.point-in-time-restore-data-query-round-trip rds.read-replica-promote-lifecycle rds.cluster-read-replica-promotion
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.point-in-time-restore-data-query-round-trip|run#44|AssertionError|assert dst["Endpoint"]["Address"] != src_addr, (
Issue-Key: rds.read-replica-promote-lifecycle|_wait_until#19|Timeout|last = probe()
Issue-Key: rds.cluster-read-replica-promotion|_poll_until#23|Timeout|last = probe()
Issue-Commit: 97025d55c5c1c7554c95fdf98ddaa852bf21abc7

# rds: 2 net-new behavioral causes — a PITR-restored instance is published on the SAME endpoint address as its source, and the read-replica switchover/promote lifecycle never settles

> _Note: this issue was identified against the code base at commit `97025d55c5c1c7554c95fdf98ddaa852bf21abc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RestoreDBInstanceToPointInTime

## Description
Two distinct net-new root causes in the RDS instance-lifecycle surface, both grounded in failing behavioral checks of this replay. They are bundled because they belong to one service; a fixer must close BOTH before submitting.

Cause 1 of 2: RestoreDBInstanceToPointInTime publishes the restored instance on the same Endpoint.Address as the source instance.
AWS: Smithy (com-amazonaws-rds-amazonrdsv19) — RestoreDBInstanceToPointInTime takes SourceDBInstanceIdentifier + TargetDBInstanceIdentifier and returns a DBInstance; DBInstance.Endpoint is an Endpoint whose member Address is documented as "The DNS address of the DB instance". A point-in-time restore creates a NEW, separate DB instance under the new TargetDBInstanceIdentifier, and AWS gives every DB instance its OWN DNS address derived from that instance's identifier (the <identifier>.<account-suffix>.<region>.rds.amazonaws.com form). Two distinct DB instances therefore never share an Endpoint.Address: the restored instance is reachable, and is reachable separately from the source. The requirement is the DISTINCTNESS and per-instance derivation of the published address, not any particular host name, which is environment-specific.
Emulator: the restored instance's DBInstance.Endpoint.Address is byte-identical to the source instance's address — both are published as the bare loopback literal (the replay recorded 127.0.0.1 for both; the literal value is environment-specific, the collision is not). The check asserts `dst["Endpoint"]["Address"] != src_addr` and fails with "a PITR restore is a separate instance and must get its own endpoint address, not the source's".
Why: a client that restores to a point in time and then connects to the restored instance's published address is silently routed to the SOURCE instance. Every read of the restored data is served by the wrong database, so a restore-and-verify workflow (the whole point of PITR) cannot be performed, and two instances cannot be told apart by their endpoints.
Fix: the restore handler's response construction / endpoint assignment for the newly created instance — services/service-com-amazonaws-rds-amazonrdsv19/src/ops/restore_db_instance_to_point_in_time.rs — must derive the new instance's published Endpoint.Address from the new instance's own identity rather than copying the source's or emitting one shared constant for every instance.
Traces: rds.point-in-time-restore-data-query-round-trip (mismatch; assertion at check line 425).

Cause 2 of 2: the read-replica role-transition lifecycle (CreateDBInstanceReadReplica -> SwitchoverReadReplica -> PromoteReadReplica, and the Aurora ReplicationSourceIdentifier equivalent) never reaches the states AWS reports, so both lifecycle checks run to the replay cap with no verdict.
AWS: Smithy — SwitchoverReadReplica exchanges the primary and replica roles: afterwards the former replica's DBInstance carries NO ReadReplicaSourceDBInstanceIdentifier and DBInstanceStatus "available", while the former primary reports ReadReplicaSourceDBInstanceIdentifier == the former replica and returns to "available" with no PendingModifiedValues. PromoteReadReplica then removes ReadReplicaSourceDBInstanceIdentifier from the promoted instance, leaving it standalone and "available"; a replicating replica reports StatusInfos[0].Status == "replicating". The same contract holds for an Aurora secondary cluster created with ReplicationSourceIdentifier and then promoted to standalone.
Emulator: neither check ever observes those transitions — both are cut off at the harness per-check cap ("Check timed out after 60.0s") while polling for them. The checks' own waits are generous (2400 s / 900 s per state), so the cap is reached while an expected state has not been published, and the suite records no response for the asserted transition. Hypothesis (the model does not state it and no refusal was recorded): the replica/secondary never leaves its creating/modifying state, or the switchover/promote never re-writes ReadReplicaSourceDBInstanceIdentifier on the two instances, so the poll predicate can never become true.
Why: a client cannot complete a managed role transition. SwitchoverReadReplica/PromoteReadReplica appear to be accepted but the described state never changes, so any orchestration that waits for the promoted instance to become standalone (failover drills, replica cut-overs, blue/green style promotions) hangs forever instead of converging.
Fix: the read-replica state machine behind these operations — the handlers for CreateDBInstanceReadReplica, SwitchoverReadReplica and PromoteReadReplica in services/service-com-amazonaws-rds-amazonrdsv19/src/ops/ and the instance/cluster state they mutate — must drive the replica to "available" with StatusInfos "replicating", then exchange/clear ReadReplicaSourceDBInstanceIdentifier and settle both instances back to "available" so the documented terminal states are observable.
Traces: rds.read-replica-promote-lifecycle (mismatch, oracle-ee instance replica switchover+promote), rds.cluster-read-replica-promotion (mismatch, aurora-mysql secondary cluster promotion).

Related open issues that do NOT carry these checks' replay keys (the suite did not mark these checks as owned): 20261121T105515Z-rds-5-net-new-behavioral-causes-aurora-cluster.md (Aurora cluster endpoints published on an unserved '.rds.localhost' host) and 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md (the published endpoint of a served engine is the core listener's). Cause 1 here is specifically the COLLISION of a restored instance's address with its source's, which neither of those states.

## Evidence
Cause 1 — rds.point-in-time-restore-data-query-round-trip:
  File "check_rds_point_in_time_restore_data_query_round_trip.py", line 425, in run
    assert dst["Endpoint"]["Address"] != src_addr, (
  AssertionError: a PITR restore is a separate instance and must get its own endpoint address, not the source's (127.0.0.1)
Expected: DBInstance.Endpoint.Address of the instance created under TargetDBInstanceIdentifier differs from the source instance's Endpoint.Address (each instance gets its own DNS address derived from its own identifier).
Actual: both instances publish the identical address (127.0.0.1 in this run), so the restored instance has no address of its own. Every earlier validation assertion of that check (the two RestoreTime window-bound refusals and both InvalidParameterCombination messages) passed, so the divergence is isolated to the restored instance's published endpoint.

Cause 2 — rds.read-replica-promote-lifecycle and rds.cluster-read-replica-promotion:
  Error: Check timed out after 60.0s
Expected (read-replica-promote-lifecycle): after SwitchoverReadReplica(replica) the replica reports no ReadReplicaSourceDBInstanceIdentifier and DBInstanceStatus "available", the former primary reports ReadReplicaSourceDBInstanceIdentifier == replica and "available" with no PendingModifiedValues, and after PromoteReadReplica(former primary) it carries no ReadReplicaSourceDBInstanceIdentifier; the replica reports StatusInfos[0].Status == "replicating" while replicating.
Expected (cluster-read-replica-promotion): an Aurora secondary cluster created with ReplicationSourceIdentifier becomes a standalone cluster after promotion.
Actual: no observation of any of those states was captured in either check — both were killed at the harness cap while polling (the checks themselves allow 900–2400 s per state), i.e. the asserted transition was never published.

## Steps to Reproduce
Cause 1:
1. CreateDBInstance (mysql, db.t3.micro, BackupRetentionPeriod=1) in a DB subnet group; wait for "available" and note DBInstance.Endpoint.Address (call it SRC).
2. RestoreDBInstanceToPointInTime(SourceDBInstanceIdentifier=<src>, TargetDBInstanceIdentifier=<new>, UseLatestRestorableTime=true, DBInstanceClass="db.t3.micro").
3. DescribeDBInstances(DBInstanceIdentifier=<new>) once it is "available" and compare DBInstance.Endpoint.Address to SRC — the emulator returns the same address; AWS returns a different, per-instance address.

Cause 2:
1. CreateDBInstance (oracle-ee, db.m5.large, BackupRetentionPeriod=1), wait "available".
2. CreateDBInstanceReadReplica(SourceDBInstanceIdentifier=<primary>, ReplicaMode="mounted"), wait for the replica to be "available" with StatusInfos[0].Status == "replicating"; ModifyDBInstance(BackupRetentionPeriod=1, ApplyImmediately=true) on the replica.
3. SwitchoverReadReplica(DBInstanceIdentifier=<replica>) and poll DescribeDBInstances on both instances for the exchanged ReadReplicaSourceDBInstanceIdentifier values.
4. PromoteReadReplica(DBInstanceIdentifier=<former primary>) and poll for it to become standalone — the documented states are never reached.
5. Aurora variant: CreateDBCluster(aurora-mysql) as primary, CreateDBCluster(ReplicationSourceIdentifier=<primary arn>) as secondary, then promote the secondary and poll for a standalone cluster.
