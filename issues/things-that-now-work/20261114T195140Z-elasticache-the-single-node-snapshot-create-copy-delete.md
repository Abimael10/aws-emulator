Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes|_poll#27|Timeout|time.sleep(interval_s)
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ElastiCache: the single-node snapshot create/copy/delete round trip never converges — every status wait costs a client its whole poll interval, so the check dies at the 60 s budget with no assertion reached (1 cause, 1 check)

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSnapshots

## Description
One net-new root cause (Cause 1 of 1), grounded in the single failing elasticache check of this replay that the report marked neither ALREADY FILED nor OWNED.

Related issues that do NOT cover this check in this replay, named here so a fixer can join them up: open issue 20261110T144301Z-elasticache-multi-step-lifecycle-flows-never-converge-every.md lists this check id in its Issue-Trace-Ids but keys it to the frame `_poll#19|Timeout|time.sleep(interval_s)`, and this replay emits a bare per-check budget expiry with no frame, so the key does not match and the report lists the check unowned. Open issue 20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md argues the OPPOSITE direction of the same knob (transient windows unobservable) and carries none of this check's id. Resolved issue 20261112T074018Z-elasticache-lifecycle-flows-never-converge-inside-a-client.md claimed this exact flow as its Cause 1 and is marked fixed, yet the flow still never converges in this replay — the check is not refused by an error code, it is still killed by the budget inside its status polls, so this is filed as the still-open convergence defect rather than as a new one.

Cause 1 of 1: a resource whose scheduled transition has already fallen due is still reported with its TRANSIENT status to a client's read, and only materialised as settled by a LATER call, so every wait in a multi-step flow costs the client a full poll interval; the six sequential waits of the snapshot create/copy/delete round trip therefore exceed any client budget. Failing check: elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes.
  AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json) — Snapshot$SnapshotStatus is documented "creating | available | restoring | copying | deleting"; CreateSnapshot -> Snapshot with errors CacheClusterNotFoundFault {CacheClusterNotFound,404}, InvalidCacheClusterStateFault {InvalidCacheClusterState,400}, SnapshotAlreadyExistsFault {400}, SnapshotFeatureNotSupportedFault {400}, SnapshotQuotaExceededFault {400}; CopySnapshot -> Snapshot with SnapshotNotFoundFault {404}, InvalidSnapshotStateFault {InvalidSnapshotState,400}, SnapshotAlreadyExistsFault {400}; DeleteSnapshot -> Snapshot with SnapshotNotFoundFault {404}, InvalidSnapshotStateFault {400}; DescribeSnapshots -> {Snapshots, Marker} with SnapshotNotFoundFault {404}. Recorded behavior for the sequencing: a snapshot of a single-node redis cluster reports `creating` and then settles `available` carrying exactly one NodeSnapshot and the source CacheClusterId; CopySnapshot yields an INDEPENDENT snapshot that reports creating/copying then available, records the SOURCE CacheClusterId and one NodeSnapshot, and survives DeleteSnapshot of its source; DeleteSnapshot then removes each snapshot (DescribeSnapshots answers 404 SnapshotNotFoundFault / an empty list) and DeleteCacheCluster removes the cluster (DescribeCacheClusters then answers 404 CacheClusterNotFound). The requirement is the status progression, the one-NodeSnapshot-per-node projection and the copy's independence from its source; the snapshot names, cluster ids and timestamps of any one run are per-run values, not part of it.
  Emulator: the check is killed at the suite's 60 s per-check budget with NO assertion evaluated — not the one-NodeSnapshot assertion on the source, not the copy's SnapshotStatus right after CopySnapshot, not the copy's CacheClusterId, not the coexistence of both snapshots, not the survivor remaining `available`, and none of the three final absences. Detail, verbatim and complete: `Error: Check timed out after 60.0s`. Mechanism in this checkout, from the state's own contract: services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state.rs:426 `pub const CLUSTER_TRANSITION: Duration = Duration::from_millis(500);` and :436 `SNAPSHOT_TRANSITION` likewise 500 ms, documented at :414-425 as "The due instant is stored on the record and materialised by whichever later call reads it; a REPORT issued inside the span answers with the transient status, because nothing of this service sleeps for a transition it has scheduled." A client's first poll therefore lands inside the span, observes the transient value and sleeps its whole interval; the flow chains six sequential waits (cluster available, source snapshot available, copy available, source snapshot gone, copy gone, cluster gone) at interval_s=15, i.e. 90 s of pure sleep against a 60 s budget. Hypothesis: that per-wait interval loss, chained six times, is the whole cost; the alternative — that one specific status (the copy's `available`, or a post-delete absence) is never reached at all — cannot be separated from this output, because no assertion in the check fired.
  Why: a client written against AWS polls SnapshotStatus and CacheClusterStatus on an AWS-sized interval (tens of seconds). Against this emulator every such wait costs that full interval instead of being satisfied by the first poll issued after the transition is due, so a snapshot-then-copy-then-two-deletes-then-cluster-delete flow takes minutes of wall clock and is indistinguishable from a snapshot that never settles or a delete that never completes. SDK waiters and CI pipelines time out, and nothing about the copy's independence from its source is ever observable.
  Fix: the status-settling path of the elasticache crate's state (the settle/transition helpers around CLUSTER_TRANSITION / SNAPSHOT_TRANSITION) — a record whose transition instant has already passed must read back SETTLED to the FIRST call that observes it, while the create/copy/delete RESPONSE keeps reporting the transient status that other checks assert; the DescribeSnapshots and DescribeCacheClusters projections and the CopySnapshot / DeleteSnapshot handlers must then carry the flow to its end (one NodeSnapshot per node, the SOURCE CacheClusterId on the copy, the copy unaffected by its source's deletion, and each delete ending in absence).
  Traces: elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes; mechanism citation services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state.rs:414-426 and :428-436.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: 99/186 passed (53.2%), 71 mismatched / 16 errored / 0 skipped; elasticache 90/156 (58%) with 66 failures. Of the 81 listed failing checks, exactly one is bound to no open issue and is not marked ALREADY FILED — the one below.

1. [mismatch] elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes
   Detail, verbatim and complete: `Error: Check timed out after 60.0s` — no traceback and no assertion text, so the per-check wall-clock budget killed the check before any of its own assertions ran.
   Flow from the check source (CHECK='snapshot-of-a-single-node-cluster-copies-and-deletes', GROUP='snapshot', SERVICES=['elasticache'], tags axis:happy-path, cap:c4, concern:representation, plane:control):
     CreateCacheSubnetGroup over the run network -> CreateCacheCluster(Engine='redis', CacheNodeType='cache.t4g.micro', NumCacheNodes=1, SnapshotRetentionLimit=0) -> _wait_cluster_available (DescribeCacheClusters until CacheClusterStatus == 'available', interval_s=15) -> CreateSnapshot(CacheClusterId, SnapshotName=<src>) -> _wait_snapshot_available(<src>, interval_s=15) -> assert Snapshot.CacheClusterId == <cluster> and len(NodeSnapshots) == 1 -> CopySnapshot(SourceSnapshotName=<src>, TargetSnapshotName=<cpy>) -> assert the returned SnapshotStatus is in ('creating','copying','available') -> _wait_snapshot_available(<cpy>) -> assert the copy records the SOURCE CacheClusterId and 1 NodeSnapshot -> assert both snapshots coexist -> DeleteSnapshot(<src>) -> _wait_snapshot_deleted(<src>) -> assert the copy survives and is still 'available' -> DeleteSnapshot(<cpy>) -> _wait_snapshot_deleted(<cpy>) -> DeleteCacheCluster -> _wait_cluster_deleted.
   Six sequential waits at interval_s=15 cost 90 s of sleep on their own if each wait misses on its first poll, which exceeds the 60 s budget before any assertion is reached.

Expected vs actual:
   Expected: each wait is satisfied by the first poll issued after the 500 ms transition is due, so the whole round trip completes in seconds and the check evaluates its assertions — source snapshot 'available' with CacheClusterId == <cluster> and exactly 1 NodeSnapshot; the CopySnapshot response status in creating/copying/available; the copy 'available' with the SOURCE CacheClusterId and 1 NodeSnapshot; both snapshots present at once; after DeleteSnapshot(<src>) the source absent (DescribeSnapshots 404 SnapshotNotFoundFault / empty list) and the copy still 'available'; after DeleteSnapshot(<cpy>) the copy absent; after DeleteCacheCluster the cluster absent (DescribeCacheClusters 404 CacheClusterNotFound).
   Actual: `Error: Check timed out after 60.0s`, no assertion evaluated — every status poll missed on its first read and slept its full 15 s interval.

Mechanism evidence from the checkout: services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state.rs:426 `pub const CLUSTER_TRANSITION: Duration = Duration::from_millis(500);` and :436 `pub const SNAPSHOT_TRANSITION: Duration = Duration::from_millis(500);`, with the doc comment at :414-425: "The due instant is stored on the record and materialised by whichever later call reads it; a REPORT issued inside the span answers with the transient status, because nothing of this service sleeps for a transition it has scheduled."

Model evidence (smithy_query on services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json): Snapshot$SnapshotStatus documentation "The status of the snapshot. Valid values: creating | available | restoring | copying | deleting."; CreateSnapshot -> CreateSnapshotResult{Snapshot}, errors CacheClusterNotFoundFault {CacheClusterNotFound,404}, InvalidCacheClusterStateFault {InvalidCacheClusterState,400}, SnapshotAlreadyExistsFault {400}, SnapshotFeatureNotSupportedFault {400}, SnapshotQuotaExceededFault {400}; CopySnapshot -> CopySnapshotResult{Snapshot}, errors SnapshotNotFoundFault {404}, InvalidSnapshotStateFault {InvalidSnapshotState,400}, SnapshotAlreadyExistsFault {400}; DeleteSnapshot -> DeleteSnapshotResult{Snapshot}, errors SnapshotNotFoundFault {404}, InvalidSnapshotStateFault {400}; DescribeSnapshots -> {Snapshots, Marker}, errors SnapshotNotFoundFault {404}, CacheClusterNotFoundFault {CacheClusterNotFound,404}.

Ownership evidence: the report lists this check with neither an "ALREADY FILED" nor an "OWNED by" marker. 20261110T144301Z-elasticache-multi-step-lifecycle-flows-never-converge-every.md carries `Issue-Key: elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes|_poll#19|Timeout|time.sleep(interval_s)` and 20261112T074018Z-elasticache-lifecycle-flows-never-converge-inside-a-client.md (resolved) carries `Issue-Key: ...|_poll#27|Timeout|time.sleep(interval_s)`; this replay produces no frame at all for the check, so neither key matches it.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1.
2. CreateCacheSubnetGroup over the run's subnets, then CreateCacheCluster(CacheClusterId=<id>, Engine='redis', CacheNodeType='cache.t4g.micro', NumCacheNodes=1, SnapshotRetentionLimit=0, CacheSubnetGroupName=<sng>) and immediately poll DescribeCacheClusters(CacheClusterId=<id>) in a loop. Expect the FIRST poll issued after the 500 ms transition is due to report CacheClusterStatus 'available'; observe 'creating' on that poll and 'available' only on a later read, so a client polling at 15 s pays 15 s for the wait.
3. Once available: CreateSnapshot(CacheClusterId=<id>, SnapshotName=<src>) -> poll DescribeSnapshots(SnapshotName=<src>) to SnapshotStatus 'available' and check CacheClusterId == <id> and exactly one NodeSnapshot.
4. CopySnapshot(SourceSnapshotName=<src>, TargetSnapshotName=<cpy>): the response SnapshotStatus must be one of creating/copying/available. Poll DescribeSnapshots(SnapshotName=<cpy>) to 'available' and check it records the SOURCE CacheClusterId (<id>) and one NodeSnapshot.
5. Check both <src> and <cpy> are present at once. DeleteSnapshot(SnapshotName=<src>) and poll <src> to absence (404 SnapshotNotFoundFault / empty list); <cpy> must still be present and 'available'. DeleteSnapshot(SnapshotName=<cpy>) and poll to absence. DeleteCacheCluster(CacheClusterId=<id>) and poll to absence (404 CacheClusterNotFound).
6. Expect the whole sequence to complete in seconds, every wait satisfied by its first poll once the transition is due, with the create/copy/delete RESPONSES still reporting their transient statuses. Observe instead that each of the six waits misses on its first read and sleeps 15 s, and the check is killed at the 60 s per-check budget with none of the assertions above reached.
7. Re-replay elasticache.snapshot-of-a-single-node-cluster-copies-and-deletes: it must reach and pass its own assertions instead of dying at the per-check budget.

## Resolution

The whole round trip is served, and two suites of the owning crate now drive it over the wire
and fail if any of it regresses:

- `services/service-com-amazonaws-elasticache-amazonelasticachev9/tests/all/lifecycle_convergence.rs`
  (`the_snapshot_round_trip_converges_by_polling_each_wait_out`) drives the recorded flow the way
  a client drives it — cluster available, snapshot `available` with the source `CacheClusterId`
  and exactly one `NodeSnapshot`, the copy's `creating` answer, the copy `available` recording the
  SOURCE cluster, both snapshots coexisting, the source removed and the copy still `available`,
  the copy removed, the cluster removed and then 404 `CacheClusterNotFound`.
- `services/service-com-amazonaws-elasticache-amazonelasticachev9/tests/all/snapshot_source_projection.rs`
  spans the same rule over the sources `CreateSnapshot` accepts (a cache cluster, a replication
  group, a group partitioned across shards), which is where this fix found a real divergence:
  `CreateSnapshot` naming an EXISTING replication group was refused 404
  `ReplicationGroupNotFoundFault`, so half of the one rule the flow rests on was unreachable.
  It now mints that snapshot from the primary of each shard.

What the report attributes the budget expiry to is not a defect of this emulator. A record whose
transition instant has passed is materialised by the FIRST call that reports it
(`src/state/store.rs` `settled_snapshot_mut` / `settled_cluster_mut` / `purge_deleted_snapshots`,
pinned by `tests/all/first_read_sees_the_settled_state.rs`). The replayed check still reports
`Check timed out after 60.0s` because it sleeps its own poll interval once per transition it
starts: six waits at `interval_s=15` is 90 s of sleep against a 60 s budget, whatever the
emulator's span is. Making the poll issued microseconds after the starting call report the
settled value would need a zero-length transient span, which would delete the `creating` /
`modifying` / `deleting` windows the recording itself shows and the state gates that go with them
— the direction open issue
`20261114T131431Z-elasticache-transient-lifecycle-windows-are-not-observable-creating.md` files
against — and a report materialising a change the clock does not yet owe would be a read that
writes.

