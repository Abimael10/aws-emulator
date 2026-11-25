Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 2d06ccff52785b63f85b33377a06ca3208ec3de1

# CreateQueue persists a queue with no compute node group associations, although the operation's own documentation requires 1 or more

_Filed: 2026-11-25T01:07:45Z_
_Operation: CreateQueue | Class: validation_

> _Note: this issue was identified against the code base at commit `2d06ccff52785b63f85b33377a06ca3208ec3de1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateQueue`'s own Smithy documentation states the requirement in the imperative: "You must associate 1 or more compute node groups with the queue." The handler deliberately does not enforce it — a comment in the middle of the validation block declares that the count is "not a ground for refusal" — and it then stores whatever the request wrote, including an absent member, an empty list, or entries that name no group at all. The result is a 200 and a persisted, ACTIVE queue that assigns jobs to nothing, which `GetQueue`/`ListQueues` then report as a healthy queue.

The only rule the handler applies to that member is a reference check on the entries that DO name a group (`cluster.available_compute_node_group`), so `computeNodeGroupConfigurations: []`, `computeNodeGroupConfigurations: [{}]` and an absent member all sail through.

Note on where the check belongs: the crate used to hold this bound (`state::queue_association_violation` / `MIN_QUEUE_ASSOCIATIONS`, quoted in the open issue `20261122T041529Z-pcs-createqueue-with-a-nonexistent-clusteridentifier-is-refused-validationexception`) and applied it BEFORE the store was locked, which made a request naming an unknown cluster answer `ValidationException` instead of `ResourceNotFoundException`. That issue's own fix text asks for the bound to be applied AFTER `clusterIdentifier` is resolved ("resolve clusterIdentifier against the store (the existing unresolved-cluster arm) before applying the service's own queue-association lower bound"), not for the bound to disappear. The current code dropped the bound entirely; restoring it must keep the ordering the handler has today — cluster resolution first, then the association minimum — so the two findings do not fight each other.

## Evidence

Smithy, `smithy_query(query="doc", shape="CreateQueue")` — the operation documentation states the rule itself:
> "Creates a job queue. You must associate 1 or more compute node groups with the queue. You can associate 1 compute node group with multiple queues."

Handler declines to enforce it — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:234-241:
```
        // HOW MANY groups the request associates is not a ground for refusal: a queue
        // "assigns jobs to associated compute node groups"
        // (`CreateQueueRequest$computeNodeGroupConfigurations`), and that member is
        // OPTIONAL on a list the model gives no minimum length, whose own
        // `ComputeNodeGroupConfiguration` carries one optional member — so a request that
        // names no group asks for a queue that assigns jobs to nothing YET, which
        // `UpdateQueue` is the operation for. Only the groups a configuration NAMES are
        // checked, below, once the cluster this create is scoped to has resolved.
```
and stores the list unchecked — create_queue.rs:381-392 (only entries that NAME a group are validated, the count never is):
```
        let configurations = input.compute_node_group_configurations.unwrap_or_default();
        for configuration in &configurations {
            let Some(named) = configuration.compute_node_group_id.as_deref() else {
                continue;
            };
```
then create_queue.rs:415 persists it: `compute_node_group_configurations: configurations,`.

Test coverage that pins the current acceptance, and the reason it states — tests/all/cluster_lifecycle/queue_associations.rs:7-15:
> "RULE under test: a queue stores the associations its request wrote and HOW MANY groups they name is never itself a ground for refusal — `computeNodeGroupConfigurations` is an optional member of an unbounded list whose `ComputeNodeGroupConfiguration` carries one optional member ... so a create that names no group at all is served and stores exactly that, and `UpdateQueue` is what associates a group later."
and tests/all/cluster_lifecycle/queue_associations.rs:94-141 (`a_create_that_names_no_compute_node_group_is_served_and_stores_what_it_wrote`) asserts `status, 200` for the rows "the member absent altogether", "an empty list", "one entry naming no group", "three entries, none naming a group". That stated reason rests ONLY on the member's traits (optional, no `@length`); it never addresses the operation documentation sentence above, which states the requirement the model's traits do not carry.

Prior shape of the rule in this crate, quoted by an existing issue (issues/20261122T041529Z-pcs-createqueue-with-a-nonexistent-clusteridentifier-is-refused-validationexception.md): "state::queue_association_violation / MIN_QUEUE_ASSOCIATIONS = 1 ... surfaced through state::field_validation as the constraint clause \"Member must have length greater than or equal to 1\"" — the helper and constant no longer exist anywhere in src/ (grep for `queue_association` / `MIN_QUEUE_ASSOCIATIONS` finds nothing; state.rs:200 now declares only `MIN_SUBNET_IDS`).

## Steps to Reproduce

1. Boot the emulator and call CreateCluster(clusterName="queue-host", scheduler={type:SLURM,version:"24.11"}, size=SMALL, networking={subnetIds:[<a resolvable subnet>]}) -> 200.
2. Call CreateQueue(clusterIdentifier="queue-host", queueName="orphan") with no `computeNodeGroupConfigurations` member at all (and again with `computeNodeGroupConfigurations: []`, and again with `[{}]`).
3. Observed: HTTP 200 each time; the response carries `queue.computeNodeGroupConfigurations: []` (or the group-less entries as written) and the queue is persisted.
4. Call GetQueue(clusterIdentifier="queue-host", queueIdentifier="orphan"): 200, status ACTIVE, still no compute node group association — a queue that can assign jobs to nothing.
5. Expected per the operation documentation ("You must associate 1 or more compute node groups with the queue"): the create is refused and nothing is persisted; ListQueues reports no such queue.
6. Ordering constraint for the fix: keep the existing cluster resolution first — CreateQueue(clusterIdentifier="pcs_22222222222222222222" (no such cluster), queueName="orphan", no computeNodeGroupConfigurations) must still answer ResourceNotFoundException "AWS PCS couldn't find the cluster you specified.", not the association refusal.
