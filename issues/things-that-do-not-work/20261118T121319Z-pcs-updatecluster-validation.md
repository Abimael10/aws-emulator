Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: ee24c053d2f664e1aece2bc382274e6b27f60b27

# UpdateCluster never checks that the cluster's queues and compute node groups are ACTIVE

_Filed: 2026-11-18T12:13:19Z_
_Operation: UpdateCluster | Class: validation_

> _Note: this issue was identified against the code base at commit `ee24c053d2f664e1aece2bc382274e6b27f60b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

UpdateCluster's own documentation states two preconditions. The handler implements the first (the cluster's own status) and nothing of the second: it never looks at the cluster's children, so an update is accepted while an associated compute node group is in a non-ACTIVE status, and the cluster is moved to UPDATING and the change persisted.

This is reachable with no concurrency: DeleteComputeNodeGroup leaves the group `DELETING` with `PendingChange::MarkDeleted`, and the settle pass at the top of the NEXT request turns it into `DELETED` and keeps the record for one more request. An UpdateCluster issued at that point sees a stored, non-ACTIVE compute node group (GetComputeNodeGroup/ListComputeNodeGroups still report it) and proceeds anyway.

The crate already has the children gate for the analogous rule on the delete path (`ClusterRecord::blocking_children`, used by DeleteCluster); the update path calls nothing equivalent.

## Evidence

Smithy (smithy_query doc, UpdateCluster): "<note> <p>You can only update clusters that are in <code>ACTIVE</code>, <code>UPDATE_FAILED</code>, or <code>SUSPENDED</code> state. All associated resources (queues and compute node groups) must be in <code>ACTIVE</code> state before you can update the cluster.</p> </note>"

The handler implements only the first sentence — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/update_cluster.rs:236-243:
```
        // "You can only update clusters that are in ACTIVE, UPDATE_FAILED, or SUSPENDED
        // state": one still provisioning, already updating, or on its way out conflicts.
        if !crate::state::UPDATABLE_CLUSTER_STATUSES.contains(&cluster.status.as_str()) {
            return Err(UpdateClusterErrorModeled::ConflictException(
                crate::state::cluster_not_active(&cluster.id),
            )
            .into());
        }
```
Nothing after that line reads `cluster.queues` or `cluster.compute_node_groups` (update_cluster.rs:245-264 applies `scheduler`/`slurmConfiguration` and sets STATUS_UPDATING). The constant's doc quotes only the first half of the note — src/state.rs:108-110:
```
/// The cluster statuses an update can start from: "You can only update clusters that are in
/// ACTIVE, UPDATE_FAILED, or SUSPENDED state" (`UpdateCluster`'s own documentation).
pub const UPDATABLE_CLUSTER_STATUSES: &[&str] = &[STATUS_ACTIVE, "UPDATE_FAILED", "SUSPENDED"];
```
Sibling handler with the children gate the crate already owns — src/ops/delete_cluster.rs:182-189:
```
        // "You must delete all queues and compute node groups associated with the cluster
        // first": a cluster that still holds either is in an unstable state to delete.
        if cluster.blocking_children() > 0 {
```
(`ClusterRecord::blocking_children`, src/state.rs:440-450.)

Why the non-ACTIVE child is observable — src/state.rs:99-102: "the group is reported `DELETED` for one request and then forgotten, while a queue and a cluster go straight from `DELETING` to gone", materialised by `apply_pending` (src/state.rs:866-871).

Test coverage: tests/all/cluster_update_and_endpoints.rs exercises UpdateCluster against a cluster with no children only (`update_cluster_refuses_a_cluster_it_cannot_find_or_cannot_update`, line 521; `update_cluster_persists_the_slurm_options_the_request_carries`, line 389) — no case holds a non-ACTIVE queue or node group, so nothing pins the current acceptance.

## Steps to Reproduce

1. CreateCluster(clusterName="upd", ...) -> 200 (CREATING).
2. CreateComputeNodeGroup(clusterIdentifier="upd", computeNodeGroupName="nodes", ...) -> 200 (the settle pass has made the cluster ACTIVE).
3. DeleteComputeNodeGroup(clusterIdentifier="upd", computeNodeGroupIdentifier="nodes") -> 200; the group is DELETING with MarkDeleted pending.
4. UpdateCluster(clusterIdentifier="upd", slurmConfiguration={scaleDownIdleTimeInSeconds:700}). The settle pass at the top of this request makes the group DELETED (still stored).
5. Expected per the documented precondition ("All associated resources (queues and compute node groups) must be in ACTIVE state before you can update the cluster"): the update is refused with the modelled ConflictException. Observed: HTTP 200 with cluster status UPDATING and the new value persisted; GetComputeNodeGroup on "nodes" in the same window still reports a non-ACTIVE (DELETED) group.
