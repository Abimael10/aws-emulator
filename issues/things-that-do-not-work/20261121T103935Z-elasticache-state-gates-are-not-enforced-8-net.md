Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.cache-cluster-duplicate-id elasticache.replication-group-duplicate elasticache.replica-count-state-gating elasticache.concurrent-replica-count-increase-decrease-contention elasticache.delete-replication-group-snapshotting-state-blocked elasticache.migration-complete-without-active-migration-rejected elasticache.migration-state-transitions elasticache.online-migration-lifecycle elasticache.replication-group-migration-lifecycle elasticache.concurrent-modify-user-group-membership-race elasticache.test-failover-state-gating
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.cache-cluster-duplicate-id|_error#8|AssertionError|raise AssertionError(f"expected a ClientError, but the call SUCCEEDED and returned {result!r}")
Issue-Key: elasticache.replication-group-duplicate|_error#8|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: elasticache.replica-count-state-gating|_error#10|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: elasticache.concurrent-replica-count-increase-decrease-contention|run#12|AssertionError|assert len(successes) == 1, f"expected exactly one winning replica-count call, got {len(successes)}: {results}"
Issue-Key: elasticache.delete-replication-group-snapshotting-state-blocked|_error#14|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Key: elasticache.migration-complete-without-active-migration-rejected|run#18|AssertionError|assert second_code == "InvalidReplicationGroupState", (
Issue-Key: elasticache.migration-state-transitions|run#11|AssertionError|assert (code, message) == ("InvalidReplicationGroupState", _START_NOT_ALLOWED_MESSAGE), (
Issue-Key: elasticache.online-migration-lifecycle|run#23|AssertionError|assert code == "InvalidReplicationGroupState", code
Issue-Key: elasticache.replication-group-migration-lifecycle|_error#17|AssertionError|raise AssertionError(f"expected this call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: elasticache.concurrent-modify-user-group-membership-race|run#20|AssertionError|assert failures[0][1] == "InvalidUserGroupState", f"expected InvalidUserGroupState, got {outcomes}"
Issue-Key: elasticache.test-failover-state-gating|_error#17|AssertionError|raise AssertionError("expected a ClientError but the call SUCCEEDED")
Issue-Commit: 0046bef99fc51ab9ab45bb6f9c8d889fe477827c

# ElastiCache state gates are not enforced: 8 net-new causes where a transition AWS refuses is accepted with 200, or refused with the wrong code and message

> _Note: this issue was identified against the code base at commit `0046bef99fc51ab9ab45bb6f9c8d889fe477827c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReplicationGroup

## Description
Eight net-new root causes in the elasticache emulator, grounded in the elasticache failing checks the replay report binds to NO open issue (not marked ALREADY FILED and not OWNED). A fixer must close all EIGHT before submitting. They share one theme — the control plane does not gate a mutation on the resource's current lifecycle state, and where it does gate it answers the modeled fault shape name or a neighbouring error instead of AWS's awsQuery code and sentence — but each is a distinct operation/gate and each has its own failing check(s).

Cause 1 of 8: DeleteCacheCluster on a cluster that is still `creating` is ACCEPTED.
  AWS: Smithy DeleteCacheCluster declares InvalidCacheClusterStateFault with awsQueryError code `InvalidCacheClusterState`, HTTP 400. AWS refuses the delete with the fixed sentence pair `Can only delete cache clusters with state in: available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster <CacheClusterId> has state: <status>.` — the permitted-state list is fixed text; the two variable parts are the requested CacheClusterId and the cluster's current status from the service's state (`creating` here).
  Emulator: the call returns HTTP 200 with a CacheCluster projection whose CacheClusterStatus is already `deleting`; no error at all is raised.
  Why: a client that creates a cluster and (correctly) expects the id to be unusable until it settles sees its delete accepted, and the cluster it intended to keep is torn down. Retry/guard logic written against AWS's InvalidCacheClusterState never fires, and a test of such logic passes against the emulator while failing on AWS.
  Fix: the DeleteCacheCluster handler's state gate — it must consult the cluster's current status before mutating it and emit InvalidCacheClusterState with the permitted-state-list message for any status outside that list.
  Note: open issue 20261119T112538Z-elasticache-3-net-new-causes-the-served-cache.md reported THIS check with a different signature (the gate fired, with the emulator's own 'is in creating state, not available.' wording). That signature no longer occurs: the gate now does not fire at all, so this is a distinct defect in the same handler and the open issue does not cover it.
  Traces: elasticache.cache-cluster-duplicate-id (check line 90); handler services/service-com-amazonaws-elasticache-amazonelasticachev9/src/ops/delete_cache_cluster.rs.

Cause 2 of 8: ModifyReplicationGroup on a replication group that has not settled is ACCEPTED.
  AWS: Smithy ModifyReplicationGroup declares InvalidReplicationGroupStateFault with awsQueryError code `InvalidReplicationGroupState`, HTTP 400. A group that is not `available` refuses every modify with the wholly fixed sentence `Replication group must be in available state to modify.` (no variable parts).
  Emulator: CreateReplicationGroup's group is readable as `available` immediately and the follow-up ModifyReplicationGroup returns HTTP 200, with the new ReplicationGroupDescription already applied — there is no state at which the modify is refused.
  Why: a client that guards a modify behind AWS's refusal (or that asserts a just-created group cannot yet be reconfigured) is silently told the opposite; a reconfiguration that AWS would reject lands, so emulator-backed tests of create-then-modify sequencing prove nothing.
  Fix: ModifyReplicationGroup's handler must gate on the group's status and emit InvalidReplicationGroupState with that fixed sentence while the group is not available; the group's own state machine must also expose the non-available window it is in after create.
  Traces: elasticache.replication-group-duplicate (check line 29 via _assert_error on the ModifyReplicationGroup leg; the duplicate-create leg of the same check passes); handler .../src/ops/modify_replication_group.rs.

Cause 3 of 8: IncreaseReplicaCount / DecreaseReplicaCount apply no state gate, so replica-count changes are not serialized.
  AWS: Smithy IncreaseReplicaCount and DecreaseReplicaCount both declare InvalidReplicationGroupStateFault, awsQueryError code `InvalidReplicationGroupState`, HTTP 400. An accepted replica-count change moves the group out of `available`, and any further replica-count call while it applies is refused with the fixed sentence `Replication group must be in available state to modify.` Exactly one of two simultaneous calls is accepted; the other is refused.
  Emulator: after an accepted IncreaseReplicaCount (the response itself reports Status `modifying`), an immediately following DecreaseReplicaCount still returns HTTP 200; and a simultaneous increase+decrease pair BOTH return 200 (observed outcomes: ('ok','increase',3) and ('ok','decrease',1)), leaving the final member count a function of whichever write landed last.
  Why: a client doing a controlled scale-out cannot detect that a competing scale-in was accepted — AWS guarantees mutual exclusion here, so a client never has to reconcile two concurrent targets. Against the emulator both callers believe they won and the group converges to a count neither requested.
  Fix: the shared replica-count path (both handlers) must take the group's state as a gate and a lock: refuse with InvalidReplicationGroupState unless the group is available, and transition it out of available before answering.
  Traces: elasticache.replica-count-state-gating (check line 45), elasticache.concurrent-replica-count-increase-decrease-contention (check line 64); handlers .../src/ops/increase_replica_count.rs, .../src/ops/decrease_replica_count.rs.

Cause 4 of 8: DeleteReplicationGroup is accepted while a snapshot of a member cluster is in progress.
  AWS: Smithy DeleteReplicationGroup declares InvalidReplicationGroupStateFault, awsQueryError code `InvalidReplicationGroupState`, HTTP 400. While a member cache cluster is `snapshotting`, the delete is refused with `Cache cluster <member-cache-cluster-id> is not in a valid state to be deleted.` — fixed text apart from the member cluster id, which comes from the group's own MemberClusters in service state. Once the snapshot reaches `available` the same delete succeeds.
  Emulator: with a CreateSnapshot in flight against the group's sole member, DeleteReplicationGroup returns HTTP 200 and the group's Status flips to `deleting`; the in-progress snapshot's node is destroyed under it.
  Why: a client that takes a manual backup before a destructive change relies on AWS refusing the delete until the backup is durable. The emulator lets the delete race the snapshot, so the one safety property the sequence exists for is unobservable.
  Fix: DeleteReplicationGroup's handler must consult the snapshot state of every member cluster (not only the group's own Status) and emit the InvalidReplicationGroupState refusal naming the member cluster.
  Note: this gate was previously reported and marked fixed in 20261109T010905Z-elasticache-deletereplicationgroup-never-applies-the-snapshot-in-progress.md and 20261109T193317Z-elasticache-deletereplicationgroup-answers-notimplemented-instead-of-its-two.md; the operation no longer answers NotImplemented, but the gate itself still does not fire.
  Traces: elasticache.delete-replication-group-snapshotting-state-blocked (check line 108 via _assert_error); handler .../src/ops/delete_replication_group.rs.

Cause 5 of 8: a duplicate StartMigration answers ReplicationGroupAlreadyUnderMigrationFault instead of the state refusal AWS serves.
  AWS: Smithy StartMigration declares BOTH ReplicationGroupAlreadyUnderMigrationFault and InvalidReplicationGroupStateFault (awsQueryError `InvalidReplicationGroupState`, HTTP 400). On the wire, a second StartMigration issued while the first is applying is refused as a state conflict: code `InvalidReplicationGroupState` with the wholly fixed sentence `Replication group must be in available or migrating status to start migration.`
  Emulator: code `ReplicationGroupAlreadyUnderMigrationFault` with the message `Replication group <ReplicationGroupId> is already under migration.` — a different error class, and a message whose only fixed part is the emulator's own wording (the id is echoed from the request).
  Why: a client that classifies migration errors by AWS's code sees an error shape AWS does not serve for this transition, so its retry-when-still-applying branch (keyed on InvalidReplicationGroupState) never runs and a transient conflict is treated as a terminal 'already migrating' condition.
  Fix: StartMigration's handler must answer the state-gate error for a group that is not in available/migrating status, rather than the already-under-migration fault.
  Traces: elasticache.migration-complete-without-active-migration-rejected (check line 91), elasticache.migration-state-transitions (check line 63), elasticache.online-migration-lifecycle (check line 89); handler .../src/ops/start_migration.rs.

Cause 6 of 8: StartMigration does not move the group out of `available`, so DeleteReplicationGroup during a live migration is ACCEPTED.
  AWS: an accepted StartMigration transitions the replication group to `modifying`, and DeleteReplicationGroup is then refused with code `InvalidReplicationGroupState` (HTTP 400) and the message `Replication group <ReplicationGroupId> has status modifying which is not valid for deletion.` — fixed text except the group id (from the request) and the status (from service state).
  Emulator: after an accepted StartMigration the group stays readable as `available` and the delete returns HTTP 200 with Status `deleting`, destroying the group mid-migration.
  Why: a client cannot see that a migration is in flight, and a teardown issued during one succeeds where AWS protects the group. Any workflow that relies on the delete being blocked until the migration ends behaves differently against the emulator.
  Fix: StartMigration must record the migration in the group's state and transition its Status, and DeleteReplicationGroup's state gate must refuse a group whose status is not valid for deletion with the status-naming message above.
  Traces: elasticache.replication-group-migration-lifecycle (check line 76); handlers .../src/ops/start_migration.rs and .../src/ops/delete_replication_group.rs.

Cause 7 of 8: concurrent ModifyUserGroup answers DuplicateUserName instead of the user-group state refusal.
  AWS: Smithy ModifyUserGroup declares InvalidUserGroupStateFault (awsQueryError `InvalidUserGroupState`, HTTP 400) alongside DuplicateUserNameFault. Two simultaneous ModifyUserGroup calls adding the same user are serialized: one is accepted and returns Status `modifying`, and the loser is refused as a state conflict — `User group <UserGroupId> has status modifying. Only active user groups can be modified.` Fixed text apart from the group id (from the request) and the status (from service state).
  Emulator: one call returns 200 with Status `modifying` and the other is refused with code `DuplicateUserName` and the message `User <UserId> is already a member of user group <UserGroupId>.` — the wrong error class for this transition, produced by evaluating membership against state the winner has already mutated.
  Why: a client retrying on AWS's InvalidUserGroupState (the documented transient) instead sees DuplicateUserName, which it must read as a permanent, caller-caused conflict; a converging RBAC update becomes a hard failure, and the emulator never exercises the modifying window at all.
  Fix: ModifyUserGroup's handler must gate on the group's status before evaluating membership, emitting InvalidUserGroupState with the status-naming sentence while the group is not `active`.
  Traces: elasticache.concurrent-modify-user-group-membership-race (check line 87); handler .../src/ops/modify_user_group.rs.

Cause 8 of 8: a second TestFailover on the same node group right after an accepted one is ACCEPTED.
  AWS: Smithy TestFailover declares TestFailoverNotAvailableFault, awsQueryError code `TestFailoverNotAvailableFault`, HTTP 400. ElastiCache enforces a per-node-group cooldown: a TestFailover issued while one is already running on that node group is refused with the wholly fixed message `Test failover already in progress` (no variable parts), and the member roles are unchanged by the rejected call. The cooldown is not visible as a Status transition on the group or the node group.
  Emulator: the second TestFailover returns successfully — no error is raised — so the cooldown does not exist in the emulator's state.
  Why: a client (or a chaos-test harness) that expects back-to-back failover requests to be throttled sees every request accepted, so it can drive repeated role changes AWS would refuse, and its handling of TestFailoverNotAvailableFault is never exercised.
  Fix: TestFailover's handler must record an in-progress test per node group in the replication group's state and refuse a concurrent request with TestFailoverNotAvailableFault and that fixed message.
  Note: open issue 20261120T174937Z-elasticache-state-gate-refusals-diverge-a-conflicting-reshard.md reports a second TestFailover answering the WRONG CODE (InvalidReplicationGroupState); the signature here is different — the call is not refused at all — so that issue does not cover this check.
  Traces: elasticache.test-failover-state-gating (check line 68; the NodeGroupNotFoundFault leg of the same check passes); handler .../src/ops/test_failover.rs.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: total 138/194 (71.1%); elasticache 121/161 (75%), 40 failures, of which 29 are OWNED by open issues or ALREADY FILED for unimplemented stubs. The eleven checks below are the net-new ones, clustered into the eight causes above.

Cause 1 — [mismatch] elasticache.cache-cluster-duplicate-id
check_elasticache_cache_cluster_duplicate_id.py:90 `refused = _error(lambda: ec.delete_cache_cluster(CacheClusterId=cluster_id))`
AssertionError: expected a ClientError, but the call SUCCEEDED and returned {'CacheCluster': {... 'CacheClusterStatus': 'deleting' ...}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}
Expected (AWS): HTTP 400, Error.Code 'InvalidCacheClusterState', Error.Message 'Can only delete cache clusters with state in: available, failed, incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster <id> has state: creating.'

Cause 2 — [mismatch] elasticache.replication-group-duplicate
check_elasticache_replication_group_duplicate.py:29 -> :25
AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'ReplicationGroup': {... 'Description': 'nope', 'Status': 'available' ...}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}
Expected (AWS): HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Replication group must be in available state to modify.'

Cause 3 — [mismatch] elasticache.replica-count-state-gating
check_elasticache_replica_count_state_gating.py:45 -> :133
AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'ReplicationGroup': {... 'Status': 'modifying' ...}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}} (DecreaseReplicaCount issued while the accepted IncreaseReplicaCount was still applying)
[mismatch] elasticache.concurrent-replica-count-increase-decrease-contention
check_elasticache_concurrent_replica_count_increase_decrease_contention.py:64
AssertionError: expected exactly one winning replica-count call, got 2: [('ok', 'increase', 3, None, None), ('ok', 'decrease', 1, None, None)]
Expected (AWS) for the loser: HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Replication group must be in available state to modify.'

Cause 4 — [mismatch] elasticache.delete-replication-group-snapshotting-state-blocked
check_elasticache_delete_replication_group_snapshotting_state_blocked.py:108 -> :103
AssertionError: expected a rejection, but the call SUCCEEDED and returned {'ReplicationGroup': {... 'Status': 'deleting', 'NodeGroups': [{'Status': 'deleting' ...}] ...}, 'ResponseMetadata': {'HTTPStatusCode': 200 ...}}
Expected (AWS): HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Cache cluster <member-cluster-id> is not in a valid state to be deleted.'

Cause 5 — [mismatch] elasticache.migration-complete-without-active-migration-rejected
check_elasticache_migration_complete_without_active_migration_rejected.py:91
AssertionError: unexpected rejection code for a duplicate StartMigration: 'ReplicationGroupAlreadyUnderMigrationFault'
[mismatch] elasticache.migration-state-transitions
check_elasticache_migration_state_transitions.py:63
AssertionError: duplicate StartMigration while the first applies: code 'ReplicationGroupAlreadyUnderMigrationFault', message 'Replication group <rg-id> is already under migration.'
[mismatch] elasticache.online-migration-lifecycle
check_elasticache_online_migration_lifecycle.py:89
AssertionError: ReplicationGroupAlreadyUnderMigrationFault
Expected (AWS) in all three: HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Replication group must be in available or migrating status to start migration.'

Cause 6 — [mismatch] elasticache.replication-group-migration-lifecycle
check_elasticache_replication_group_migration_lifecycle.py:76 -> :131
AssertionError: expected this call to be rejected, but it SUCCEEDED and returned {'ReplicationGroup': {... 'Status': 'deleting' ...}} (DeleteReplicationGroup issued right after an accepted StartMigration)
Expected (AWS): HTTP 400, Error.Code 'InvalidReplicationGroupState', Error.Message 'Replication group <rg-id> has status modifying which is not valid for deletion.'

Cause 7 — [mismatch] elasticache.concurrent-modify-user-group-membership-race
check_elasticache_concurrent_modify_user_group_membership_race.py:87
AssertionError: expected InvalidUserGroupState, got [('error', 'DuplicateUserName', 'User <user-id> is already a member of user group <group-id>.'), ('ok', 'modifying')]
Expected (AWS) for the loser: HTTP 400, Error.Code 'InvalidUserGroupState', Error.Message 'User group <group-id> has status modifying. Only active user groups can be modified.'

Cause 8 — [mismatch] elasticache.test-failover-state-gating
check_elasticache_test_failover_state_gating.py:68 -> :105
AssertionError: expected a ClientError but the call SUCCEEDED (second TestFailover on the same NodeGroupId)
Expected (AWS): HTTP 400, Error.Code 'TestFailoverNotAvailableFault', Error.Message 'Test failover already in progress'

In every cause the cluster/group/user ids, node ids, hosts, ports, request ids and timestamps in the captured responses are per-run values and are NOT part of the requirement; the HTTP status, the error code, the fixed message text and which request/state values fill its variable slots are.

## Steps to Reproduce
Cause 1: CreateCacheCluster(CacheClusterId=<id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheNodes=1, CacheSubnetGroupName=<group>); immediately call DeleteCacheCluster(CacheClusterId=<id>) while the cluster is still 'creating'. Expect HTTP 400 InvalidCacheClusterState with the permitted-state-list sentence; observe HTTP 200 and CacheClusterStatus='deleting'.

Cause 2: CreateReplicationGroup(ReplicationGroupId=<rg>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheClusters=1, CacheSubnetGroupName=<group>); immediately call ModifyReplicationGroup(ReplicationGroupId=<rg>, ReplicationGroupDescription='nope', ApplyImmediately=True). Expect HTTP 400 InvalidReplicationGroupState 'Replication group must be in available state to modify.'; observe HTTP 200 with the new description applied.

Cause 3: create a 1-node replication group with AutomaticFailoverEnabled=False, wait for 'available', then IncreaseReplicaCount(NewReplicaCount=1, ApplyImmediately=True) and, without waiting, DecreaseReplicaCount(NewReplicaCount=0, ApplyImmediately=True). Expect the second call to be refused HTTP 400 InvalidReplicationGroupState 'Replication group must be in available state to modify.'; observe HTTP 200. Then, on an available 3-node group, fire IncreaseReplicaCount(NewReplicaCount=3) and DecreaseReplicaCount(NewReplicaCount=1) simultaneously: expect exactly one success and one InvalidReplicationGroupState; observe two successes.

Cause 4: create a 1-node replication group, wait for 'available', CreateSnapshot(CacheClusterId=<member>, SnapshotName=<snap>), then while DescribeSnapshots reports the snapshot not yet 'available' call DeleteReplicationGroup(ReplicationGroupId=<rg>). Expect HTTP 400 InvalidReplicationGroupState 'Cache cluster <member> is not in a valid state to be deleted.' and the group still present; observe HTTP 200 and Status='deleting'.

Cause 5: create a 2-node replication group with AutomaticFailoverEnabled=True, wait for 'available', StartMigration(CustomerNodeEndpointList=[{'Address':'203.0.113.10','Port':6379}]), then immediately StartMigration again with the same arguments. Expect HTTP 400 InvalidReplicationGroupState 'Replication group must be in available or migrating status to start migration.'; observe ReplicationGroupAlreadyUnderMigrationFault 'Replication group <rg> is already under migration.'

Cause 6: same group and StartMigration as Cause 5, then call DeleteReplicationGroup(ReplicationGroupId=<rg>). Expect HTTP 400 InvalidReplicationGroupState 'Replication group <rg> has status modifying which is not valid for deletion.' and DescribeReplicationGroups still returning the group; observe HTTP 200 and Status='deleting'.

Cause 7: CreateUser twice (users A and B, Engine='redis', NoPasswordRequired=True), CreateUserGroup(UserGroupId=<g>, Engine='redis', UserIds=['default', A]), wait for Status='active', then issue two simultaneous ModifyUserGroup(UserGroupId=<g>, UserIdsToAdd=[B]) calls from a barrier. Expect one success (Status='modifying') and one HTTP 400 InvalidUserGroupState 'User group <g> has status modifying. Only active user groups can be modified.'; observe the loser answering DuplicateUserName.

Cause 8: create a 2-node replication group with AutomaticFailoverEnabled=True, wait for 'available', TestFailover(ReplicationGroupId=<rg>, NodeGroupId=<the group's NodeGroupId>) (accepted), then immediately TestFailover again with the same NodeGroupId. Expect HTTP 400 TestFailoverNotAvailableFault 'Test failover already in progress' with the member roles unchanged; observe a successful response.

Re-replay the checks listed in trace_ids to confirm the fix.
