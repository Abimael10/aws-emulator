Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.replication-group-not-found-semantics elasticache.delete-replication-group-wrong-state-rejected elasticache.replication-group-migration-lifecycle elasticache.replication-group-lifecycle elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot elasticache.auth-token-dual-validity-window
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.replication-group-not-found-semantics|_assert_error#10|AssertionError|assert actual == message, f"{expected}: expected message {message!r}, got {actual!r}"
Issue-Key: elasticache.delete-replication-group-wrong-state-rejected|run#9|AssertionError|assert refused == (
Issue-Key: elasticache.replication-group-migration-lifecycle|run#20|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: elasticache.replication-group-lifecycle|run#20|AssertionError|assert deleting is not None and deleting["Status"] == "deleting", deleting
Issue-Key: elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot|run#12|AssertionError|assert len(by_ip) == _N_SHARDS, (
Issue-Key: elasticache.auth-token-dual-validity-window|_redis_ping#18|Timeout|time.sleep(5.0)
Issue-Commit: 1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46

# ElastiCache replication-group delete/not-found refusals, the deleting window, shard endpoint identity and the auth-token rotate flow diverge from AWS (6 causes)

> _Note: this issue was identified against the code base at commit `1b54d60a21bdcb9d9cbbbe8d2d80ba69001d0d46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReplicationGroup

## Description
Six independent net-new root causes in the elasticache service, grounded in the six failing elasticache checks this run's replay report binds to no open issue and does not mark ALREADY FILED. ALL SIX must be closed before submitting; each cause below names its own failing check ids.

Related open issues report neighbouring families but own a different set of replay keys and none of the check ids below: 20261109T151249Z-elasticache-state-gate-and-limit-refusals-answer-one.md and 20261108T152921Z-elasticache-refusals-diverge-from-aws-on-error-code.md (state-gate/refusal text on other operations), 20261109T193317Z-elasticache-deletereplicationgroup-answers-notimplemented-instead-of-its-two.md (the snapshot/migration state gates of DeleteReplicationGroup, not its message text), 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md and 20261110T144301Z-elasticache-multi-step-lifecycle-flows-never-converge-every.md (same convergence mechanism, other check ids), 20261111T005655Z-elasticache-data-plane-still-unreachable-and-multi-wait.md (lists elasticache.auth-token-dual-validity-window under its Cause 3 but its recorded replay key `_redis_ping#18|Timeout` no longer matches this run's failure, so the suite binds this check to no issue).

Cause 1 of 6: DeleteReplicationGroup's not-found refusal carries DescribeReplicationGroups' message instead of its own.
AWS: Smithy: DeleteReplicationGroup errors include ReplicationGroupNotFoundFault (httpError 404, awsQueryError code `ReplicationGroupNotFoundFault`), doc "The specified replication group does not exist.". Recorded behavior: AWS's not-found text is PER-OPERATION for this resource — DeleteReplicationGroup answers the fixed sentence `Specified replication group does not exist` (no variable part: it does not name the id), while DescribeReplicationGroups answers `ReplicationGroup <ReplicationGroupId> not found.` (fixed text "ReplicationGroup " + the requested id + " not found."), TestFailover `Replication group <ReplicationGroupId> does not exist.` and ModifyReplicationGroupShardConfiguration `Replication group '<ReplicationGroupId>' not found`. In the same check the Describe, TestFailover and shard-configuration texts matched; only the Delete one diverged.
Emulator: DeleteReplicationGroup on an unknown id answers code ReplicationGroupNotFoundFault (correct) with the DESCRIBE message — `ReplicationGroup <requested id> not found.` — i.e. one shared not-found sentence for every replication-group operation.
Why: a client that branches on the refusal text (log parsers, idempotent-teardown helpers, test assertions on the service's documented message) sees a message AWS never emits for this call, and cannot tell a Delete refusal from a Describe refusal.
Fix: the not-found path of the DeleteReplicationGroup handler (its own message, not the shared resolver's).
Traces: elasticache.replication-group-not-found-semantics.

Cause 2 of 6: DeleteReplicationGroup on a group that is still `creating` names the group instead of the member cache cluster whose state blocks the delete.
AWS: Smithy: InvalidReplicationGroupStateFault (httpError 400, awsQueryError code `InvalidReplicationGroupState`), doc "The requested replication group is not in the available state.". Recorded behavior: the refusal names the FIRST MEMBER CLUSTER, which AWS ids as `<ReplicationGroupId>-001`: fixed text "Cache cluster " + that member cluster id + " is not in a valid state to be deleted." The group stays `creating` and is not deleted.
Emulator: the code is right (InvalidReplicationGroupState) but the message is `Replication group <ReplicationGroupId> is not in a valid state to be deleted.` — the group's id, not the blocking member cluster's.
Why: AWS's message tells the caller WHICH node is blocking the delete, which is what a retry/teardown loop logs and acts on; the emulator's text names a resource that is not the blocker and never matches a client's expected string.
Fix: the state gate in the DeleteReplicationGroup handler — build the refusal message from the blocking member cache cluster's id.
Traces: elasticache.delete-replication-group-wrong-state-rejected.

Cause 3 of 6: the same state gate answers one generic sentence for every status — it omits the status that makes the delete invalid.
AWS: same error shape/code as Cause 2 (400 InvalidReplicationGroupState). Recorded behavior: DeleteReplicationGroup against a group whose Status is `modifying` (here, during an online migration) is refused with fixed text "Replication group " + the ReplicationGroupId + " has status " + the group's current Status + " which is not valid for deletion." — i.e. the status is a variable part taken from the group's state, and the sentence differs from the `creating` form of Cause 2.
Emulator: `Replication group <ReplicationGroupId> is not in a valid state to be deleted.` — the same single sentence it uses for `creating`, with no status in it.
Why: a caller cannot learn from the refusal which state it must wait out, and the string does not match what AWS sends, so state-machine clients that key off the status in the message misbehave.
Fix: the DeleteReplicationGroup state gate must emit AWS's per-state wording (the member-cluster form of Cause 2 for a group still creating, the "has status <Status> which is not valid for deletion." form for a group being modified).
Traces: elasticache.replication-group-migration-lifecycle.

Cause 4 of 6: an accepted DeleteReplicationGroup makes the group vanish immediately — there is no observable `deleting` window.
AWS: Smithy: ReplicationGroup$Status is documented "The current state of this replication group - creating, available, modifying, deleting, create-failed, snapshotting", and DeleteReplicationGroup's output carries the ReplicationGroup. Recorded behavior: the delete returns the group with Status `deleting`, and a DescribeReplicationGroups issued right after the delete STILL returns the group with Status `deleting` until teardown completes; only then does the id stop resolving (404 ReplicationGroupNotFoundFault).
Emulator: the DescribeReplicationGroups issued immediately after a successful DeleteReplicationGroup returns the group not at all — either 404 ReplicationGroupNotFoundFault or an empty ReplicationGroups list (the check's helper maps both to None) — so the documented `deleting` state is never observable.
Why: every SDK waiter and teardown loop written against AWS polls for `deleting` -> gone. A group that disappears between one call and the next makes the delete indistinguishable from "the group was never there", and any assertion or orchestration step on the `deleting` status is unreachable.
Fix: the DeleteReplicationGroup handler and the replication-group state machine — put the record into `deleting` and keep it describable for the transition, instead of removing it inside the delete call.
Traces: elasticache.replication-group-lifecycle.

Cause 5 of 6: every shard of a cluster-mode replication group advertises the SAME address, so three shards are one backend wearing three names.
AWS: Smithy: Endpoint is "Represents the information required for client programs to connect to a cache node" with Endpoint$Address "The DNS hostname of the cache node"; NodeGroupMember$ReadEndpoint is "The information required for client programs to connect to a node", and NodeGroup/NodeGroupMember represent "a single node within a node group (shard)". Recorded behavior: in a 3-shard (cluster-mode enabled) group, each shard's member node endpoint hostname resolves to a DISTINCT address inside the cluster's VPC, and the engine's `CLUSTER SLOTS` names those same per-shard addresses, one owner per slot range, partitioning 0-16383.
Emulator: the three shard node hostnames all resolve to one and the same address (this run: 127.0.0.1 for node names 0001/0002/0003, differing only by port). The concrete host/port values are per-run and are not the requirement; the requirement is one distinct, in-VPC address per shard node, matching what CLUSTER SLOTS advertises.
Why: a cluster-aware client builds its slot map from these endpoints. With one address behind every shard the keyspace is flat: -MOVED redirects, per-shard ownership and -CROSSSLOT refusals cannot be exercised or verified at all, and a client that works here will mis-route against AWS.
Fix: the replication-group/cache-cluster endpoint projection for cluster-mode groups (one address per node group member, inside the group's VPC) together with the data plane that must serve each of those addresses. Related fixed issue 20261109T151249Z-elasticache-serves-endpoints-it-does-not-listen-on.md addressed endpoint publication generally; shard-address distinctness is still absent.
Traces: elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot.

Cause 6 of 6: the auth-token dual-validity (ROTATE) flow never reaches a single assertion inside a client's budget.
AWS: recorded behavior only. CreateCacheSubnetGroup over two subnets, then CreateReplicationGroup(Engine=redis, CacheNodeType=cache.t3.micro, NumCacheClusters=1, TransitEncryptionEnabled=true, AuthToken=<old>, CacheSubnetGroupName=<group>, SecurityGroupIds=[...]) settles Status `available`; during an AuthTokenUpdateStrategy=ROTATE both the old and the new token authenticate against the live endpoint, and once SET finalizes the rotation only the new token does while the old is refused by AUTH (-WRONGPASS/-NOAUTH).
Emulator: the check ends with `Check timed out after 60.0s` and NO assertion ever fires — the flow's chained status waits (and the endpoint connects behind them) consume the whole per-check budget. Hypothesis: the same settling mechanism other convergence issues describe (a transition that is already due is only materialised by a LATER read, so each wait costs a client its full poll interval) plus an endpoint that accepts no connection; the replay cannot separate the two because nothing in the check asserted.
Why: a client cannot confirm that a transit-encrypted, auth-token-protected replication group became usable, and the documented dual-validity window of an auth-token rotation — the whole point of ROTATE, which exists so clients can migrate credentials with no downtime — is unobservable.
Fix: the status-settling path of the elasticache crate's state (a resource whose transition is due must read back settled to the FIRST call that observes it) and the auth-token rotation path of ModifyReplicationGroup together with the endpoint it must serve.
Traces: elasticache.auth-token-dual-validity-window.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: 101/186 passed overall; service heading `### elasticache` 92/156 (64 failed). The six failures below are the elasticache failures the report binds to no open issue and does not mark ALREADY FILED.

Cause 1 — elasticache.replication-group-not-found-semantics (verdict: mismatch), check file line 73 (the DeleteReplicationGroup assertion):
  expected: code ReplicationGroupNotFoundFault, message 'Specified replication group does not exist'
  actual:   AssertionError: ReplicationGroupNotFoundFault: expected message 'Specified replication group does not exist', got 'ReplicationGroup sim2real-behav-erg-missing-442d8d4e26ad not found.'
  (the same check's DescribeReplicationGroups / TestFailover / ModifyReplicationGroupShardConfiguration not-found texts passed; the id in the message is a per-run value.)

Cause 2 — elasticache.delete-replication-group-wrong-state-rejected (verdict: mismatch), check file line 44, after the group was confirmed Status=='creating':
  expected: ('InvalidReplicationGroupState', 'Cache cluster <rg-id>-001 is not in a valid state to be deleted.')
  actual:   AssertionError: expected InvalidReplicationGroupState with its exact message, got ('InvalidReplicationGroupState', 'Replication group sim2real-ec-wrongstate-0fb77881c733 is not in a valid state to be deleted.')

Cause 3 — elasticache.replication-group-migration-lifecycle (verdict: mismatch), check file line 79:
  AssertionError: expected message 'Replication group sim2real-mig-252329351436 has status modifying which is not valid for deletion.', got 'Replication group sim2real-mig-252329351436 is not in a valid state to be deleted.'
  (fixed text is "Replication group " + id + " has status " + Status + " which is not valid for deletion."; the id and the status are values from the request and the emulator's state.)

Cause 4 — elasticache.replication-group-lifecycle (verdict: mismatch), check file line 124, immediately after `ec.delete_replication_group(ReplicationGroupId=rg_id)` succeeded on an `available` group:
  assert deleting is not None and deleting["Status"] == "deleting" -> AssertionError: None
  i.e. DescribeReplicationGroups(ReplicationGroupId=<id>) right after the delete yielded no record at all. Everything before it passed: the create answered Status 'creating', the group reached 'available' with one node group ('available') and one member cluster, and ModifyReplicationGroup's new description read back.

Cause 5 — elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot (verdict: mismatch), check file line 1276:
  AssertionError: [runner-in-vpc] the 3 shard node names resolved to only 1 distinct addresses: {'0001': '127.0.0.1', '0002': '127.0.0.1', '0003': '127.0.0.1'}. Distinct shards sharing one address is a single backend wearing three names, which is the flat-keyspace failure this check exists to catch
  The check then goes on to require CLUSTER SLOTS to name those same per-shard addresses and to agree with the control plane's slot ranges; it never gets that far. Corroborating projections from sibling elasticache failures in the same run show every node endpoint as Address '127.0.0.1' with only the port varying (e.g. PrimaryEndpoint 127.0.0.1:36757 / ReaderEndpoint 127.0.0.1:41657 for a two-member group).

Cause 6 — elasticache.auth-token-dual-validity-window (verdict: mismatch):
  Error: Check timed out after 60.0s — no assertion reached. Flow: CreateCacheSubnetGroup, CreateReplicationGroup(TransitEncryptionEnabled=True, AuthToken=<old>, NumCacheClusters=1), wait Status 'available', rotate the token, then AUTH with old and new tokens against the endpoint.

Model evidence (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json): DeleteReplicationGroup errors {InvalidReplicationGroupStateFault -> 400 InvalidReplicationGroupState, ReplicationGroupNotFoundFault -> 404 ReplicationGroupNotFoundFault} and output {ReplicationGroup}; ReplicationGroupNotFoundFault doc 'The specified replication group does not exist.'; InvalidReplicationGroupStateFault doc 'The requested replication group is not in the available state.'; ReplicationGroup$Status doc lists 'deleting'; NodeGroupMember doc 'Represents a single node within a node group (shard).' with ReadEndpoint 'The information required for client programs to connect to a node'.

## Steps to Reproduce
Common setup: boot the emulator from this checkout and create an ElastiCache cache subnet group over two subnets of a VPC (CreateCacheSubnetGroup).

Cause 1: DeleteReplicationGroup(ReplicationGroupId='<an id that does not exist>'). Expect 404 ReplicationGroupNotFoundFault with message exactly 'Specified replication group does not exist'; observe 'ReplicationGroup <id> not found.'. Re-replay elasticache.replication-group-not-found-semantics.

Cause 2: CreateReplicationGroup(ReplicationGroupId=<id>, Engine='redis', CacheNodeType='cache.t3.micro', NumCacheClusters=1, AutomaticFailoverEnabled=False, CacheSubnetGroupName=<group>); while DescribeReplicationGroups still reports Status 'creating', call DeleteReplicationGroup(ReplicationGroupId=<id>). Expect 400 InvalidReplicationGroupState with 'Cache cluster <id>-001 is not in a valid state to be deleted.'; observe the group-named generic sentence. Re-replay elasticache.delete-replication-group-wrong-state-rejected.

Cause 3: create a 2-node group with AutomaticFailoverEnabled=True, wait 'available', StartMigration so the group reports Status 'modifying', then DeleteReplicationGroup. Expect 400 InvalidReplicationGroupState with 'Replication group <id> has status modifying which is not valid for deletion.'; observe the same generic sentence as Cause 2. Re-replay elasticache.replication-group-migration-lifecycle.

Cause 4: create a single-node group, wait Status 'available', call DeleteReplicationGroup (it must answer the ReplicationGroup with Status 'deleting'), then IMMEDIATELY call DescribeReplicationGroups(ReplicationGroupId=<id>). Expect the group still returned with Status 'deleting', then (after the teardown) 404 ReplicationGroupNotFoundFault. Observe no record on the first read. Re-replay elasticache.replication-group-lifecycle.

Cause 5: create a cluster-mode-enabled replication group with NumNodeGroups=3, wait 'available', read each member cluster's node endpoint from DescribeCacheClusters/DescribeReplicationGroups and resolve each hostname from inside the cluster's VPC. Expect three distinct in-VPC addresses, and CLUSTER SLOTS on any node to name those same three addresses with a total, non-overlapping 0-16383 partition matching the control plane. Observe all three names resolving to one address. Re-replay elasticache.elasticache-cluster-mode-slot-routing-moved-and-crossslot.

Cause 6: CreateReplicationGroup(TransitEncryptionEnabled=True, AuthToken=<old>, NumCacheClusters=1, CacheSubnetGroupName=<group>, SecurityGroupIds=[<sg opened on 6379>]), poll DescribeReplicationGroups until 'available', then ModifyReplicationGroup with the new AuthToken and AuthTokenUpdateStrategy='ROTATE' and AUTH with both tokens. Expect each poll issued after the transition is due to report the settled status immediately and both tokens to authenticate during the rotation; observe the check killed at the 60 s per-check budget with no assertion reached. Re-replay elasticache.auth-token-dual-validity-window.
