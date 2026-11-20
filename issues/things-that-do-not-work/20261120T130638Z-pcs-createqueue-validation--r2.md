Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 452f77616b23bfeedd67de10b882316dc811a191

# CreateQueue creates a queue associated with NO compute node group, although the operation documents "You must associate 1 or more compute node groups with the queue"

_Filed: 2026-11-20T13:06:38Z_
_Operation: CreateQueue | Class: validation_

> _Note: this issue was identified against the code base at commit `452f77616b23bfeedd67de10b882316dc811a191`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateQueue`'s own Smithy documentation states the association as a requirement on the request: "Creates a job queue. You must associate 1 or more compute node groups with the queue. You can associate 1 compute node group with multiple queues."

The handler never checks that the request associates any group. `computeNodeGroupConfigurations` is optional in the model, and the handler turns an absent list into an empty one with `unwrap_or_default()` (create_queue.rs:360), then persists it and answers 200 with a queue whose `computeNodeGroupConfigurations` is `[]`. The loop that follows only validates the entries that ARE present, and it SKIPS an entry that carries no `computeNodeGroupId` (`else { continue; }`, create_queue.rs:362-364) — `ComputeNodeGroupConfiguration` has exactly one, optional, member — so a request carrying `computeNodeGroupConfigurations: [{}]` is accepted too and stores a configuration that names no group at all. Either way the stored queue assigns jobs to nothing: `ClusterRecord::queues_assigning_to` (state.rs:449-468) resolves each entry's id and such a queue matches no group, so it is counted as a dependent of no compute node group.

The queue is then permanently unusable and unrepairable: it settles to `ACTIVE` on the next request and `UpdateQueue` — the only operation that could add a configuration — is an unimplemented scaffold (src/ops/update_queue.rs), so no later call can give the queue a compute node group. `GetQueue`/`ListQueues` report it as a healthy `ACTIVE` queue.

This is exactly the shape of rule the crate already states for the OTHER persisted list whose request member is unbounded: `subnet_list_violation` (state.rs:1267-1278) refuses a compute node group whose stored `subnetIds` would hold fewer than `MIN_SUBNET_IDS` entries, using `length_at_least_constraint` (state.rs:1055-1060), and both `CreateComputeNodeGroup` (create_compute_node_group.rs:265-273) and `UpdateComputeNodeGroup` (update_compute_node_group.rs:249-261) ask it. `CreateQueue` already bounds the same list from ABOVE (`stored_list_violation`, create_queue.rs:210-232) — only the lower bound the operation documents is missing. Nothing in the crate states or justifies accepting a queue with no association; `ValidationException` is modelled for this operation.

## Evidence

Smithy — the operation's own documentation (`smithy_query(query="doc", shape="CreateQueue")`):
"<p>Creates a job queue. You must associate 1 or more compute node groups with the queue. You can associate 1 compute node group with multiple queues.</p>"

The handler accepts zero associations — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:355-371:
```
        // A queue assigns jobs to compute node groups of its own cluster, so a
        // configuration naming one the cluster does not hold — or one whose delete this
        // service has already accepted, and therefore is not going to hold — cannot be
        // stored: such a reference would dangle as soon as the record is forgotten, and no
        // later call could repair it.
        let configurations = input.compute_node_group_configurations.unwrap_or_default();
        for configuration in &configurations {
            let Some(named) = configuration.compute_node_group_id.as_deref() else {
                continue;
            };
            if cluster.available_compute_node_group(named).is_none() {
                return Err(CreateQueueErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::COMPUTE_NODE_GROUP, named),
                )
                .into());
            }
        }
```
and it is stored as-is — create_queue.rs:385-400: `compute_node_group_configurations: configurations,` (line 394), with `pending: Some(crate::state::PendingChange::Activate)` (line 398), so the next request reports the association-less queue `ACTIVE`.

The input member is optional, so nothing refuses it at the boundary — src/ops/types/create_queue_types.rs:19:
```
    pub compute_node_group_configurations: Option<Vec<ComputeNodeGroupConfiguration>>,
```
and the entry member is optional too (`smithy_query(query="shape", shape="ComputeNodeGroupConfiguration")`: one member, `computeNodeGroupId`, not `@required`), which is what makes `[{}]` reach the `continue` above.

The lower-bound rule the crate already owns for the sibling write path — src/state.rs:1267-1278:
```
/// The constraint clause a compute node group's `subnetIds` failed, or `None` when the
/// service accepts the list.
///
/// A STORED group's subnet list is `SubnetIdList`, modelled `@length(min: 1)`
/// (`ComputeNodeGroup$subnetIds`), while the request members that place a group target the
/// unbounded `StringList`: this is the lower bound every path that WRITES that list holds it
/// to, stated once so a create and an update cannot disagree about it.
pub fn subnet_list_violation(entries: usize) -> Option<String> {
    (entries < MIN_SUBNET_IDS).then(|| length_at_least_constraint(MIN_SUBNET_IDS))
}
```
asked by src/ops/create_compute_node_group.rs:265-273 ("A group's stored subnet list must hold at least one subnet ... — the same bound the update path holds that list to").

Why no later call repairs it: src/ops/update_queue.rs is still the scaffold (`//! SCAFFOLDED, NOT IMPLEMENTED:` / 501), so the association can never be added after the create.

Test coverage: no test pins acceptance as a RULE. The only place that sends an empty list is incidental setup for a different rule — tests/all/cluster_scoped_refusals.rs:463-473:
```
    let (status, queue) = call(
        &router,
        "CreateQueue",
        &json!({
            "clusterIdentifier": "held-cluster",
            "queueName": "work",
            "computeNodeGroupConfigurations": [],
        }),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateQueue: got {queue}");
```
(the test's stated subject is which refusal an unresolvable `clusterIdentifier` earns, not the empty list; that setup would have to name the `held-cluster`/`nodes` group the lines above it create). Every other `CreateQueue` in the suites already names a group: tests/all/cluster_lifecycle.rs:142, tests/all/delete_ordering.rs:150, tests/all/refusal_wording.rs:643,680.

## Steps to Reproduce

1. CreateCluster(clusterName="queue-host", scheduler={type:"SLURM", version:"24.11"}, size="SMALL", networking={subnetIds:[<a subnet EC2 resolves>]}) -> 200; one more request settles it ACTIVE.
2. CreateQueue(clusterIdentifier="queue-host", queueName="work") — no computeNodeGroupConfigurations member at all.
3. Expected per the operation's own documentation ("You must associate 1 or more compute node groups with the queue"): the modelled ValidationException, naming computeNodeGroupConfigurations, with nothing persisted — the way CreateComputeNodeGroup refuses an empty subnetIds through crate::state::subnet_list_violation.
4. Observed: HTTP 200 with queue.status=CREATING and queue.computeNodeGroupConfigurations=[].
5. GetQueue(clusterIdentifier="queue-host", queueIdentifier="work") -> 200, status ACTIVE, computeNodeGroupConfigurations=[] — a queue that assigns jobs to no compute node group is reported healthy.
6. Same with CreateQueue(clusterIdentifier="queue-host", queueName="work2", computeNodeGroupConfigurations=[{}]): accepted, and the stored queue carries one configuration that names no group (the `else { continue; }` branch skips it).
7. UpdateQueue(clusterIdentifier="queue-host", queueIdentifier="work", computeNodeGroupConfigurations=[{computeNodeGroupId:<a group>}]) -> 501 (the handler is still the scaffold), so the association-less queue can never be repaired.
