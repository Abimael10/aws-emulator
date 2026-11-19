Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 888a7dd41752985eb1de1aec0bdb6cbbaaae5598

# CreateQueue accepts a computeNodeGroupConfigurations entry naming a compute node group whose delete is already accepted, leaving the queue pointing at a removed group

_Filed: 2026-11-18T20:27:34Z_
_Operation: CreateQueue | Class: validation_

> _Note: this issue was identified against the code base at commit `888a7dd41752985eb1de1aec0bdb6cbbaaae5598`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

CreateQueue's reference check resolves each `computeNodeGroupConfigurations[].computeNodeGroupId` through `ClusterRecord::compute_node_group`, which applies NO status filter: it happily returns a record that is `DELETING` or `DELETED` (a group whose `DeleteComputeNodeGroup` has already been accepted and whose removal the service already owes). The queue is therefore persisted with a configuration naming that group, and one request later the settle pass forgets the group record — so the stored queue holds a configuration naming a compute node group the cluster does not hold, which is precisely the state the handler's own comment says cannot be stored. `GetQueue`/`CreateQueue` then report `computeNodeGroupConfigurations` with a dangling id for the rest of the queue's life, and no later call can repair it: the group is gone, so `DeleteComputeNodeGroup`'s dependency refusal can never fire for it again.

Every OTHER dependency path in this crate asks the question with the helper the crate already owns for exactly this: `ComputeNodeGroupRecord::is_being_deleted()` (`blocking_children`, `queues_assigning_to`). CreateQueue's path is the one that needs it and does not call it, which also makes the delete-ordering invariant one-sided: a group with an ACTIVE queue referencing it cannot be deleted, but a queue referencing a group already on its way out can be created, with no concurrency involved.

## Evidence

Handler comment states the rule, and the lookup that implements it — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:355-368:
```
        // A queue assigns jobs to compute node groups of its own cluster, so a
        // configuration naming one the cluster does not hold cannot be stored.
        let configurations = input.compute_node_group_configurations.unwrap_or_default();
        for configuration in &configurations {
            let Some(named) = configuration.compute_node_group_id.as_deref() else {
                continue;
            };
            if cluster.compute_node_group(named).is_none() {
                return Err(CreateQueueErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::COMPUTE_NODE_GROUP, named),
                )
                .into());
            }
        }
```
The lookup it calls has no status filter — src/state.rs:385-392:
```
    /// The compute node group `identifier` names — its generated id or its name.
    pub fn compute_node_group(&self, identifier: &str) -> Option<&ComputeNodeGroupRecord> {
        self.compute_node_groups.get(identifier).or_else(|| {
            self.compute_node_groups
                .values()
                .find(|group| group.name == identifier)
        })
    }
```
The helper the crate already owns for "this group is on its way out", which CreateQueue never calls — src/state.rs:545-553:
```
    /// Whether this group is already on its way out, so it blocks no delete of the
    /// cluster that holds it.
    ...
    pub fn is_being_deleted(&self) -> bool {
        self.status == STATUS_DELETING || self.status == STATUS_DELETED
    }
```
Sibling paths that DO call it — src/state.rs:416-435 (`queues_assigning_to`: `.filter(|queue| !queue.is_being_deleted())`) and src/state.rs:440-450 (`blocking_children`: `.filter(|group| !group.is_being_deleted())`).

Why the record is still there when CreateQueue looks, and gone one request later — src/ops/delete_compute_node_group.rs:256-261:
```
        // PCS reports the group as DELETING until the delete comes due, then DELETED for
        // one request before the record is forgotten (see
        // [`crate::state::STATUS_DELETED`]).
        group.status = crate::state::STATUS_DELETING.to_string();
        group.modified_at = crate::state::now_date_time();
        group.pending = Some(crate::state::PendingChange::MarkDeleted);
```
and src/state.rs:866-872 (`apply_pending`: `MarkDeleted` -> `STATUS_DELETED` then `Some(PendingChange::Remove)`; `Remove` -> `true`, i.e. the owner drops the record in `ClusterRecord::settle`, src/state.rs:454-456).

The one-sided invariant — src/ops/delete_compute_node_group.rs:228-236:
```
        // "You must delete all queues associated with the compute node group first": a
        // group a queue still assigns jobs to is in an unstable state to delete, whichever
        // status that queue is in.
        if !cluster.queues_assigning_to(&group_id).is_empty() {
```

Test coverage: tests/all/delete_ordering.rs only ever creates queues BEFORE any delete (`active_queue` at line 135 is called while both cluster and group are ACTIVE, rows at lines 216-252), and `a_delete_of_a_resource_nothing_holds_is_not_found` (line 501) covers only identifiers nothing ever held. No test creates a queue against a `DELETING`/`DELETED` group, so nothing pins the current acceptance.

## Steps to Reproduce

1. CreateCluster(clusterName="q-host", scheduler={type:SLURM,version:"24.11"}, size=SMALL, networking={subnetIds:[<a subnet EC2 holds>]}) -> 200 (CREATING).
2. CreateComputeNodeGroup(clusterIdentifier="q-host", computeNodeGroupName="nodes", subnetIds=[<same subnet>], customLaunchTemplate={id:"lt-0123456789abcdef0",version:"1"}, iamInstanceProfileArn="arn:aws:iam::<account>:instance-profile/AWSPCS-demo", scalingConfiguration={minInstanceCount:0,maxInstanceCount:4}, instanceConfigs=[{instanceType:"c6i.large"}]) -> 200 (the settle pass made the cluster ACTIVE).
3. DeleteComputeNodeGroup(clusterIdentifier="q-host", computeNodeGroupIdentifier="nodes") -> 200; the group is DELETING with PendingChange::MarkDeleted.
4. CreateQueue(clusterIdentifier="q-host", queueName="work", computeNodeGroupConfigurations=[{computeNodeGroupId:"nodes"}]). The settle pass at the top of this request turns the group DELETED (record still stored). Expected: the configuration names a group the cluster is not going to hold, so the call is refused the way an unknown group id is (ResourceNotFoundException for the compute node group). Observed: HTTP 200, the queue is persisted with that configuration.
5. GetQueue(clusterIdentifier="q-host", queueIdentifier="work") -> 200: queue ACTIVE, computeNodeGroupConfigurations=[{computeNodeGroupId:"nodes"}], while GetComputeNodeGroup("nodes") now answers ResourceNotFoundException — the group record was removed by the settle pass. The queue keeps the dangling reference permanently.
