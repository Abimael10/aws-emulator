Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pcs.compute-node-group-and-queue-lifecycle
Issue-Shape-Id: com.amazonaws.pcs#AWSParallelComputingService
Issue-Key: pcs.compute-node-group-and-queue-lifecycle|_poll_until#38|Timeout|return last
Issue-Commit: 85d805405eee386b5837c9b46d90be46bccf5f7f

# pcs compute-node-group/queue lifecycle never settles for the next request: the fixed 500 ms wall-clock transition span makes a 5-wait create-then-teardown scenario unfinishable

> _Note: this issue was identified against the code base at commit `85d805405eee386b5837c9b46d90be46bccf5f7f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateComputeNodeGroup

## Description
One net-new root cause, grounded in the single surviving failing behavioral check for pcs (the other pcs failure in this run, pcs.delete-dependents-out-of-order-conflict-rejected, is marked OWNED by 20261107T094955Z-pcs-lifecycle-transitions-are-deferred-behind-a-fixed.md and is skipped). NOTE FOR THE FIXER: that open issue describes the SAME mechanism against a different check and does not carry this check's replay key, so this check is not covered by it — fixing the lifecycle clock should close both, and this issue exists so pcs.compute-node-group-and-queue-lifecycle is actually re-replayed as part of that verification.

Cause 1 of 1: a PCS resource's lifecycle transition is only observable after a fixed span of WALL-CLOCK time has elapsed, not by the time the NEXT request reads the resource, so a client that polls Get* at AWS-scale intervals burns one full interval on every single transition and a scenario that chains five waits (cluster ACTIVE, compute node group ACTIVE, queue ACTIVE, queue gone, node group DELETED) cannot complete inside any reasonable budget. Failing check: pcs.compute-node-group-and-queue-lifecycle (verdict mismatch, 'Check timed out after 60.0s').

AWS: Smithy (com.amazonaws.pcs): ClusterStatus, ComputeNodeGroupStatus and QueueStatus each declare CREATING/ACTIVE/UPDATING/DELETING/CREATE_FAILED/DELETE_FAILED/UPDATE_FAILED/SUSPENDING/SUSPENDED/RESUMING, and ComputeNodeGroupStatus additionally declares DELETED; CreateCluster/CreateComputeNodeGroup/CreateQueue answer with the resource in CREATING, and DeleteQueue/DeleteComputeNodeGroup/DeleteCluster answer with an empty body and move the resource out of ACTIVE. The model states no timing whatsoever, so for the timing itself: recorded behavior only — on AWS the transition is genuinely asynchronous because real provisioning work is happening, and the caller polls Get* until the status settles. What is NOT part of AWS's contract is a service that has already DECIDED the outcome synchronously and then withholds it from the next read for a span of its own choosing: on AWS the delay IS the work, and a client sizes its poll interval against that work.

Emulator: every create/delete records a pending change whose due instant is `Instant::now() + TRANSITION_SPAN`, with TRANSITION_SPAN fixed at 500 ms, and each handler settles only changes whose wall-clock due instant has already passed. The outcome is fully determined when the create/delete returns, yet the immediately following GetCluster/GetComputeNodeGroup/GetQueue — issued milliseconds later over loopback — still reports CREATING or DELETING (or still returns a queue that was deleted / a node group not yet DELETED). A client whose poll loop probes first and sleeps on a miss therefore pays one entire sleep interval per transition. This check pays 20 s (cluster ACTIVE) + 15 s (node group ACTIVE) + 15 s (queue ACTIVE) + 10 s (queue gone) + 15 s (node group DELETED) = at least 75 s of pure client-side sleeping against an emulator that had every one of those answers ready within half a second, and it is killed by the 60 s per-check budget. No request in the check is refused, no operation on its path is a stub, and no assertion failed: the ValidationException leg for the badly-named IAM instance profile, the ACTIVE node group's echoed subnetIds/iamInstanceProfileArn/scalingConfiguration, and the ACTIVE queue's computeNodeGroupConfigurations association are all reached and all pass before the budget runs out. What is left unverified is the tail of the scenario: the double-delete refusals ResourceNotFoundException "AWS PCS couldn't find the queue you specified." and ResourceNotFoundException "AWS PCS couldn't find the compute node group you specified." (both entirely fixed text, echoing no caller-supplied value; the resource is identified by resourceId) and the final cluster delete.

Why: a client written against AWS sizes its polling interval to AWS's provisioning time (tens of seconds), because that is the only sane choice against the real service. Against this emulator the same client spends tens of seconds observing a transition that already happened, so any workflow chaining more than two or three PCS lifecycle steps — a provisioning script, a converge loop, an integration suite, a teardown routine — runs orders of magnitude slower than the work the emulator does and trips its own deadlines. Worse, correctness that can only be demonstrated by COMPLETING a sequence (here: that a queue and node group deleted in dependency order actually disappear, and that re-deleting them is refused with the exact not-found sentences) is unreachable, so those behaviors go untested in every environment.

Fix: the lifecycle clock in this crate's state module — the TRANSITION_SPAN constant, Pending::in_a_span/is_due and the settle()/apply_pending paths the cluster, compute-node-group and queue records drive from the top of every handler. A pending transition must be settled by the time the NEXT request observes the resource rather than after a wall-clock span (e.g. advance pending changes on request arrival/sequence instead of on Instant::now()), while keeping the transient status in the response of the call that starts it: a create still answers CREATING, a delete still leaves DELETING/DELETED observable where the model declares it, so nothing asserting on the create/delete response changes.

Traces: pcs.compute-node-group-and-queue-lifecycle.

## Evidence
Behavioral suite replay against the booted emulator: pcs 11/13 passed (85%), 2 failed. One failure (pcs.delete-dependents-out-of-order-conflict-rejected) is marked OWNED and skipped; this is the remaining one.

[mismatch] pcs.compute-node-group-and-queue-lifecycle — 'Check timed out after 60.0s'. No ClientError and no AssertionError was raised: the check was killed mid-scenario, not refused, so every assertion it reached passed. Scenario and its waits (each poll helper probes FIRST and sleeps only on a miss, so a transition invisible on the probe issued right after the call costs one whole interval):
  - EC2/IAM/SSM prerequisites (VPC + subnet + default SG, launch template, two instance profiles) — fast.
  - CreateCluster -> _poll_until(GetCluster status in ACTIVE/CREATE_FAILED, interval 20 s) => 20 s.
  - CreateComputeNodeGroup with the non-AWSPCS-named profile -> ValidationException asserted (reached, passes); ListComputeNodeGroups shows no such group (reached, passes).
  - CreateComputeNodeGroup with the AWSPCS-named profile -> _poll_until(GetComputeNodeGroup status in ACTIVE/CREATE_FAILED, interval 15 s) => 15 s; the echoed subnetIds, iamInstanceProfileArn and scalingConfiguration assertions pass.
  - CreateQueue -> _poll_until(GetQueue status in ACTIVE/CREATE_FAILED, interval 15 s) => 15 s.
  - DeleteQueue -> _poll_until(GetQueue is gone, interval 10 s) => 10 s.
  - DeleteComputeNodeGroup -> _poll_until(gone or DELETED, interval 15 s) => 15 s.
Sum >= 75 s of sleeping, above the 60 s per-check budget, which is why the check dies with no assertion having failed and the double-delete legs plus the final DeleteCluster are never exercised.

Confirmed in the current checkout (why every first probe misses):
- services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/state.rs:163 — `pub const TRANSITION_SPAN: Duration = Duration::from_millis(500);`, documented as keeping "the transient state observable to the caller that acts straight after the call".
- state.rs:234-241 — `Pending::in_a_span` stores `due_at: Instant::now() + TRANSITION_SPAN`; state.rs:243-246 `is_due(now)` compares against wall clock; the settle() path called at the top of each handler materialises only due changes.
- state.rs:213-223 — PendingChange::{Activate, MarkDeleted, Remove} are exactly the five transitions this check waits on (cluster Activate, node group Activate, queue Activate, queue Remove, node group MarkDeleted).

Model cited: ClusterStatus / QueueStatus values CREATING, ACTIVE, UPDATING, DELETING, CREATE_FAILED, DELETE_FAILED, UPDATE_FAILED, SUSPENDING, SUSPENDED, RESUMING; ComputeNodeGroupStatus the same plus DELETED. No timing trait or guidance anywhere in the model.

## Steps to Reproduce
Boot the emulator and point a botocore PCS client (awsJson1_0, sigv4) at it, with EC2/IAM/SSM available for the prerequisites.

1. Create a VPC + subnet + that VPC's default security group, a launch template, and an instance profile whose role name is AWSPCS-prefixed.
2. resp = pcs.create_cluster(clusterName=<valid name>, scheduler={'type':'SLURM','version':'25.05'}, size='SMALL', networking={'subnetIds':[<subnet>],'securityGroupIds':[<sg>]}). resp['cluster']['status'] == 'CREATING' (correct).
3. IMMEDIATELY issue the next request, pcs.get_cluster(clusterIdentifier=<id>), a few milliseconds later. Observe status still 'CREATING'. Sleep 0.5 s, repeat: now 'ACTIVE'. The outcome was already decided at step 2; only wall-clock time makes it visible.
4. Repeat the same next-request observation for: CreateComputeNodeGroup (GetComputeNodeGroup still CREATING), CreateQueue (GetQueue still CREATING), DeleteQueue (GetQueue still returns the queue / DELETING), DeleteComputeNodeGroup (still DELETING, not yet DELETED).
5. Now drive the whole scenario the way a client written against AWS does — probe first, sleep an AWS-sized interval on a miss (20 s for the cluster, 15 s for node group and queue, 10 s for the queue delete, 15 s for the node-group delete). Expect it to finish as soon as the emulator has the answers (sub-second each). Observe it instead spend >= 75 s asleep and exceed the 60 s per-check budget, so the scenario never reaches its tail.
6. After the timing fix, the tail must be reachable and must hold: a second pcs.delete_queue of the deleted queue -> ResourceNotFoundException with message exactly "AWS PCS couldn't find the queue you specified."; a second pcs.delete_compute_node_group of the deleted group -> ResourceNotFoundException with message exactly "AWS PCS couldn't find the compute node group you specified." (both sentences fixed text, no caller value interpolated); then DeleteCluster succeeds and the cluster disappears.
7. The legs that already pass and must keep passing: CreateComputeNodeGroup with an instance-profile role that is neither AWSPCS-prefixed nor on the /aws-pcs/ path -> ValidationException "AWS PCS can't access a role associated with the instance profile because the role ARN is invalid. For more information, see CreateComputeNodeGroup or UpdateComputeNodeGroup in the API reference." with no node group created; the ACTIVE node group echoing the requested subnetIds, iamInstanceProfileArn and scalingConfiguration; the ACTIVE queue carrying the node group in computeNodeGroupConfigurations.

Replay to confirm: pcs.compute-node-group-and-queue-lifecycle.
