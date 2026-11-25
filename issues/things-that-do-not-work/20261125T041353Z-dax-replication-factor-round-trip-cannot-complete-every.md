Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dax.replication-factor-scaling-round-trip
Issue-Shape-Id: com.amazonaws.dax#AmazonDAXV3
Issue-Key: dax.replication-factor-scaling-round-trip|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Commit: 42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35

# dax: replication-factor round trip cannot complete — every cluster status transition is clock-owed a fixed 3 s, so a polling client pays a full poll interval per leg

> _Note: this issue was identified against the code base at commit `42bc7849dbd6fddcc9d0b96ddc9b6e42f3df4f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
IncreaseReplicationFactor

## Description
Cause 1 of 1: DAX cluster status transitions in the emulator are owed to wall-clock time (crate::state::STATUS_SETTLE_DELAY = 3 s) and are only materialised by state::settle_due() when a LATER request arrives, so no transition can ever be observed as complete by the read a client issues immediately after the mutating call. A client that drives available -> modifying -> available twice (IncreaseReplicationFactor then DecreaseReplicationFactor, each followed by DescribeClusters polling) therefore pays one full poll interval per leg and the round trip cannot finish inside the suite's per-check budget: the check dax.replication-factor-scaling-round-trip is killed at 60.0 s with three legs each costing a 20 s poll cycle (create -> available, grow -> available, shrink -> available).

AWS: Smithy (services/service-com-amazonaws-dax-amazondaxv3/smithy-model/com-amazonaws-dax-amazondaxv3.json) models Cluster$Status as a free-form string and documents IncreaseReplicationFactor as "Adds one or more nodes to a DAX cluster" and Cluster$ActiveNodes as the nodes "capable of serving requests"; neither the model nor the recording states any settling duration, so this is recorded behavior only: the recording shows the cluster reported through modifying and back to available with TotalNodes/ActiveNodes/len(Nodes) equal to the new replication factor on both legs, and both calls answered 200. Nothing in the contract requires a transition to outlive the caller's next read.

Emulator: CreateCluster, IncreaseReplicationFactor and DecreaseReplicationFactor each set cluster.status to "creating"/"modifying" and cluster.settles_at = now + 3 s; settle_due() only flips the record to "available" once some request arrives at or after that instant. The immediate DescribeClusters after each mutating call therefore always reports the transient status, and the client must sleep its whole poll interval. Replaying the round trip, the emulator never reaches the third leg's "available": the check is cut off by the 60.0 s timeout rather than answering wrongly on the wire. The 20 other dax checks, which exercise at most one transition, pass.

Why: a client that polls DescribeClusters to wait out a scale-up/scale-down sees the correct statuses but is charged a real poll interval for every transition, so a scaling round trip against the emulator costs tens of seconds of wall clock instead of settling promptly. Any caller (a test, a waiter, an orchestration step) with a bounded wait budget for "grow then shrink" times out against the emulator even though every individual response is well formed, and the operations' end state is never observed.

Fix: the clock-owed status machinery in the service's state (crate::state::STATUS_SETTLE_DELAY / ComAmazonawsDaxAmazondaxv3State::settle_due, used by the CreateCluster, IncreaseReplicationFactor and DecreaseReplicationFactor handlers and by DescribeClusters). Make the transient status observable WITHOUT costing the caller a poll interval — settle on observation (the first DescribeClusters reports modifying/creating, the next one settles it) rather than on a fixed wall-clock span, so a multi-leg round trip completes promptly while a polling client still sees the transient state.

Traces: dax.replication-factor-scaling-round-trip (mismatch: "Check timed out after 60.0s").

## Evidence
Expected (AWS / recording): IncreaseReplicationFactor(NewReplicationFactor=3) then DescribeClusters polling reaches Status="available" with TotalNodes=3, ActiveNodes=3 and len(Nodes)=3; DecreaseReplicationFactor(NewReplicationFactor=1) then reaches Status="available" with TotalNodes=1, ActiveNodes=1 and len(Nodes)=1 — the whole round trip completing within the check's budget.

Actual (emulator): the check never reaches the shrink leg's "available" and is terminated: "[mismatch] dax.replication-factor-scaling-round-trip — Error: Check timed out after 60.0s". Each leg's first DescribeClusters (issued immediately after the mutating call) reports the transient status because services/service-com-amazonaws-dax-amazondaxv3/src/state.rs:186 sets STATUS_SETTLE_DELAY = 3 s and settle_due (state.rs:1010-1048) only flips the record when a request arrives after settles_at; create_cluster.rs:444, increase_replication_factor.rs:242 and decrease_replication_factor.rs:290 all set settles_at = now + STATUS_SETTLE_DELAY. With the trace's 20 s poll interval the three legs cost ~60 s, which is the whole budget.

No status, error code or error message diverges here; every call is answered 200 with well-formed Cluster views. The divergence is that the end state of the two scaling operations is never reached within a bounded client wait.

## Steps to Reproduce
1. CreateSubnetGroup over two subnets of the account's VPC, then CreateCluster(NodeType=dax.t3.small, ReplicationFactor=1, SubnetGroupName=<group>).
2. Poll DescribeClusters(ClusterNames=[<cluster>]) every 20 s until Clusters[0].Status == "available" — note the first poll (issued ~immediately after CreateCluster) still reports "creating", so the wait costs a full 20 s.
3. IncreaseReplicationFactor(ClusterName=<cluster>, NewReplicationFactor=3), then poll DescribeClusters every 20 s until Status == "available" and TotalNodes == 3 — again the first poll reports "modifying" and the wait costs another full interval.
4. DecreaseReplicationFactor(ClusterName=<cluster>, NewReplicationFactor=1), then poll the same way until Status == "available" and TotalNodes == 1.
5. Observe the three legs together exceed a 60 s budget: the replay is killed before the final "available" is observed.
