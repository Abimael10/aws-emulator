Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: docdb-elastic.update-cluster-while-stopped-rejected
Issue-Shape-Id: com.amazonaws.docdbelastic#ChimeraDbLionfishServiceLambda
Issue-Key: docdb-elastic.update-cluster-while-stopped-rejected|_wait_for_shard_instance_count#16|Timeout|time.sleep(interval_s)
Issue-Commit: 8c92c7c2cb0689aef734153d4944deecfb6a59a7

# DocDB Elastic StopCluster/StartCluster/UpdateCluster each withhold the settled status for a fixed 3 s wall-clock window, so a polling client burns one whole poll interval per transition and the stop/reject/restart/update flow never settles

> _Note: this issue was identified against the code base at commit `8c92c7c2cb0689aef734153d4944deecfb6a59a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateCluster

## Description
Cause 1 of 1: a status transition applied to a cluster the store ALREADY holds (StopCluster -> STOPPING, StartCluster -> STARTING, UpdateCluster -> UPDATING) is pinned to a fixed wall-clock window measured from the mutating call, so every GetCluster issued inside that window — including the FIRST one a polling client issues right after the call — is still answered the transient status. Each such transition therefore costs a polling client one whole poll interval, and a flow chaining three of them cannot be observed to finish inside the check's wall-clock budget. Failing check: docdb-elastic.update-cluster-while-stopped-rejected ([mismatch] "Check timed out after 60.0s").

AWS: Smithy (com.amazonaws.docdbelastic): StopCluster POST /cluster/{clusterArn}/stop, StartCluster POST /cluster/{clusterArn}/start, UpdateCluster PUT /cluster/{clusterArn} — all three @idempotent, all three returning the @required Cluster whose @required `status` targets the Status enum (CREATING, ACTIVE, UPDATING, DELETING, STOPPING, STOPPED, STARTING, …); GetCluster GET /cluster/{clusterArn} is @readonly and returns the same required Cluster. StopCluster is documented "Stops the running elastic cluster that is specified by clusterArn. The elastic cluster must be in the available state." and StartCluster "Restarts the stopped elastic cluster that is specified by clusterARN." The model states nothing about timing. Recorded behavior only for the pacing: the mutating call's OWN response is the answer that carries the transient status (STOPPING for a stop, STARTING for a start, UPDATING for an applied update), and a subsequent GetCluster converges on the settled status — STOPPED after a stop, ACTIVE after a start or an update — with shardCapacity/shardCount and the shard list otherwise unchanged. The refusals this check exercises are recorded and are NOT the divergence: UpdateCluster against a STOPPED cluster is refused 409 ConflictException and leaves shardInstanceCount at its stored value; the emulator answers that correctly.

Emulator: `SETTLE_AFTER` is a non-zero fixed span (now 3 s) and `settle_instant()` arms `settles_at = Instant::now() + SETTLE_AFTER` on every transition of an existing record; `ClusterRecord::settle_due()` only materialises the settled status once a reader arrives after that instant. A mint is already exempt (`mint_settle_instant()` returns `Instant::now()`, so CreateCluster's record is settled by the first read), but stop, start and update are not. With the check's 20 s poll interval the arithmetic is: create -> ACTIVE on the first poll (0 s); StopCluster -> first poll answers STOPPING, STOPPED only on the second (~20 s); the 409 ConflictException and the "still STOPPED, shardInstanceCount still 1" assertions pass; StartCluster -> first poll answers STARTING, ACTIVE only on the second (~40 s); UpdateCluster(shardInstanceCount=2) -> first poll answers UPDATING, and the 60 s budget is gone before the second (~60 s). The check dies on the clock inside the positive control, never reaching the shardInstanceCount=2 assertion. No wrong status code, error code or message is observed anywhere in the flow.

Why: a client doing the documented lifecycle — stop a cluster and wait for STOPPED, restart it and wait for ACTIVE, then apply a modification and wait for it to settle — stalls one full poll interval per transition no matter how coarse or fine its polling is, and a flow chaining three or more transitions never observes the settled resource at all. StopCluster/StartCluster/UpdateCluster's end state (status STOPPED then ACTIVE, shardInstanceCount converging on the requested value, shard topology otherwise unchanged) is therefore unverifiable, and the positive control that proves the ConflictException is a state gate rather than a blanket refusal (restart, then re-apply the identical update) is unreachable.

Fix: the settle pacing in this service's state and the transition paths that arm it — services/service-com-amazonaws-docdbelastic-chimeradblionfishservicelambda/src/state.rs `SETTLE_AFTER` / `settle_instant()` / `ClusterRecord::begin_transition`, and the stop_cluster / start_cluster / update_cluster handlers that call it. A cluster should already be settled by the time of the first subsequent read, with the transient status carried only by the mutating call's own response (which these handlers already return), exactly as the mint path already does. The in-flight state refusals that read the transient status (`ensure_modifiable` for UpdateCluster, `ensure_available` for StopCluster, `ensure_startable` for StartCluster, `ensure_deletable` for DeleteCluster) must keep answering from the record's own status, so the window cannot simply be removed where those refusals depend on it.

Related: open issue 20261110T232945Z-docdb-elastic-stop-start-lifecycle-never-settles-inside.md describes the same pacing and names this check, and 20261110T154150Z-docdb-elastic-lifecycle-transitions-settle-on-a-wall.md (fixed) addressed it for the snapshot-copy/restore MINT paths only — SETTLE_AFTER was widened from 500 ms to 3 s and only mints were exempted, so the stop/start/update transitions still withhold their settled status past the first read and this check still times out.

Traces: docdb-elastic.update-cluster-while-stopped-rejected.

## Evidence
Expected (AWS / recorded behavior):
- CreateCluster(authType=PLAIN_TEXT, shardCapacity=2, shardCount=1, shardInstanceCount=1) -> cluster reaches ACTIVE.
- StopCluster(clusterArn) -> 200 with status STOPPING; a subsequent GetCluster converges on STOPPED.
- UpdateCluster(clusterArn, shardInstanceCount=2) against that STOPPED cluster -> 409 ConflictException; GetCluster still reports status STOPPED and shardInstanceCount 1.
- StartCluster(clusterArn) -> 200 with status STARTING; a subsequent GetCluster converges on ACTIVE.
- The identical UpdateCluster(shardInstanceCount=2) then succeeds and GetCluster converges on status ACTIVE with shardInstanceCount 2.

Actual (emulator):
- docdb-elastic.update-cluster-while-stopped-rejected — [mismatch] "Error: Check timed out after 60.0s". Each of the three transitions on the already-stored record (STOPPING, STARTING, UPDATING) is still reported transient to the first GetCluster that follows its mutating call, so each wait needs two polls at the check's 20 s interval: ~20 s to see STOPPED, ~40 s to see ACTIVE, and the wait for ACTIVE+shardInstanceCount=2 is still pending when the 60 s budget expires.
- Every individual HTTP call in the flow answers correctly: the 409 ConflictException for UpdateCluster on a STOPPED cluster, and 200s for create/stop/start/update. Only the settling is withheld.

Source evidence: services/service-com-amazonaws-docdbelastic-chimeradblionfishservicelambda/src/state.rs line 33 `const SETTLE_AFTER: Duration = Duration::from_secs(3)`; lines 229-253 (`begin_transition` -> `begin_transition_due_at(status, settle_instant())`); lines 262-271 (`settle_due`, which completes the move only when `Instant::now() >= due`); lines 462-464 (`settle_instant`); lines 479-481 (`mint_settle_instant`, already `Instant::now()`, which is why CreateCluster no longer costs a poll); src/ops/update_cluster.rs lines 300-302 (`record.begin_transition(state::STATUS_UPDATING)`).

## Steps to Reproduce
1. Boot the emulator and point a docdb-elastic client at it.
2. CreateCluster(clusterName=<new>, authType=PLAIN_TEXT, adminUserName=..., adminUserPassword=..., shardCapacity=2, shardCount=1, shardInstanceCount=1); poll GetCluster until ACTIVE (settles on the first poll — the mint path is already correct).
3. StopCluster(clusterArn). Immediately GetCluster: status STOPPING, and it stays STOPPING for any read issued within 3 s of the call — so a client polling on any interval >= that window burns a whole interval on this transition. Poll every 20 s until STOPPED (two polls, ~20 s).
4. UpdateCluster(clusterArn, shardInstanceCount=2): refused 409 ConflictException (correct); GetCluster still STOPPED with shardInstanceCount 1 (correct).
5. StartCluster(clusterArn); poll every 20 s until ACTIVE (two polls, ~40 s cumulative).
6. UpdateCluster(clusterArn, shardInstanceCount=2); poll GetCluster every 20 s for status ACTIVE and shardInstanceCount 2. The first poll answers UPDATING, so the settled cluster is only visible at ~60 s and the check is cut off at its 60 s budget before the positive control can be asserted.
7. Minimal reproduction of the cause: StopCluster (or StartCluster, or an applied UpdateCluster), then GetCluster immediately — the transient status is returned; the settled status only appears to a read issued more than 3 s after the mutating call.
