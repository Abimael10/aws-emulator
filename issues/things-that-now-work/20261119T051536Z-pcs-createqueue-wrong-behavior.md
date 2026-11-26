Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 250b2e17c3bded584e45f3cc61080f5579e38915

# A CreateQueue retry whose remembered clientToken points at an already-deleted queue is refused ConflictException forever, although the handler scopes that refusal to a retry naming a different cluster

_Filed: 2026-11-19T05:15:36Z_
_Operation: CreateQueue | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `250b2e17c3bded584e45f3cc61080f5579e38915`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

CreateQueue remembers `<operation>:<clientToken> -> (cluster id, queue id)` and replays the first call's queue for a retry. When the queue the token remembers has since been DELETED, the replay lookup misses and the handler falls into the `else` arm, which answers `ConflictException` ("clientToken was already used to create queue <id>"). That refusal is permanent: the token index is pruned only when the CLUSTER is removed (`forget_creates_for_cluster`), and `remember_create` never overwrites an existing entry, so for as long as the cluster lives (and the entry is not evicted by 1024 newer tokens) no CreateQueue carrying that token can ever create a queue again — the caller is stuck on a client error with no way forward except changing the token. Clients that supply a DETERMINISTIC `@idempotencyToken` (derived from a stack/queue name rather than freshly generated per call) hit this on every create-delete-recreate cycle.

Two things in this crate contradict that answer:

1. The handler's own comment states what the conflict arm is FOR — a retry naming a different cluster — and the deleted-queue case is not that case; the request names the same cluster and asks for a queue that no longer exists.
2. CreateCluster faces the identical situation (a remembered token whose resource the store no longer holds) and answers it the opposite way: it falls through the replay and carries on to create, rather than refusing.

The identical code is in CreateComputeNodeGroup (same comment, same `else` arm); it is filed here on CreateQueue because CreateComputeNodeGroup already carries an open `wrong-behavior` issue.

## Evidence

Smithy (smithy_query doc, CreateQueueRequest$clientToken): "A unique, case-sensitive identifier that you provide to ensure the idempotency of the request. Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect. If you don't specify a client token, the CLI and SDK automatically generate 1 for you." (the operation is also `@idempotent`).

The handler comment that scopes the refusal — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:282-285:
```
        // An `@idempotencyToken` retry returns the first call's result and has no
        // further effect. The token is remembered with the cluster the first call
        // created the queue in, so a retry naming a DIFFERENT cluster cannot slip past
        // the replay and create a second queue: it conflicts.
```
The code under it, which also conflicts when the cluster MATCHES and the queue is gone — create_queue.rs:286-308:
```
        if let Some(remembered) = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
            .cloned()
        {
            let first = (remembered.cluster_id.as_deref() == Some(cluster_id.as_str()))
                .then(|| {
                    state
                        .clusters
                        .get(&cluster_id)
                        .and_then(|cluster| cluster.queues.get(&remembered.resource_id))
                })
                .flatten();
            let Some(first) = first else {
                return Err(CreateQueueErrorModeled::ConflictException(ConflictException {
                    message: format!(
                        "clientToken was already used to create queue {}",
                        remembered.resource_id
                    ),
```
Why the entry outlives the queue — src/state.rs:769-782 (`forget_creates_for_cluster`): "Drop every remembered create that points at `cluster_id` or at a resource inside it, so removing a cluster releases its share of the index." with `client_tokens.retain(|_, remembered| remembered.resource_id != cluster_id && remembered.cluster_id.as_deref() != Some(cluster_id))` — it is called only from `settle` for a cluster being REMOVED (src/state.rs:795-802), never when a queue is removed (src/state.rs:456 `self.queues.retain(|_, queue| !queue.settle())`). And src/state.rs:737-741 (`remember_create`): "An existing entry is never overwritten — the token names the FIRST call's resource for as long as it is remembered".

The sibling that answers the same situation by creating — src/ops/create_cluster.rs:300-311:
```
        // An `@idempotencyToken` retry returns the first call's result and has no
        // further effect, rather than creating a second cluster or conflicting on the
        // name that call took. ...
        let remembered = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
            .map(|remembered| remembered.resource_id.clone());
        if let Some(first) = remembered.as_deref().and_then(|id| state.clusters.get(id)) {
            return Ok(CreateClusterOutput {
                cluster: Some(first.to_api()),
            });
        }
```
(no conflict arm — execution continues to the create below).

The same code in the other child create — src/ops/create_compute_node_group.rs:366-394 (comment 366-369, `ConflictException` at 383-393).

Test coverage: `grep -rn clientToken tests/` matches only first-call creates (tests/all/cluster_lifecycle.rs:793, 806, 819, 833, 911, 918); no test re-uses a token after the resource it created was deleted, so nothing pins the current refusal.

## Steps to Reproduce

1. CreateCluster(clusterName="tok-cluster", ...) -> 200; on the next request the cluster is ACTIVE.
2. CreateQueue(clusterIdentifier="tok-cluster", queueName="work", clientToken="queue-token-0001") -> 200, queue pcs_X created.
3. DeleteQueue(clusterIdentifier="tok-cluster", queueIdentifier="work") -> 200; one more request removes the record (GetQueue -> ResourceNotFoundException).
4. Re-issue the create that the deterministic token belongs to: CreateQueue(clusterIdentifier="tok-cluster", queueName="work", clientToken="queue-token-0001").
5. Observed: ConflictException "clientToken was already used to create queue pcs_X" — and every later retry with that token answers the same, because the index entry is dropped only when the CLUSTER is removed. Expected per the handler's own comment, which scopes that conflict to "a retry naming a DIFFERENT cluster", and per CreateCluster's handling of the same situation (create_cluster.rs:307: the replay is skipped when the remembered resource is no longer held and the create proceeds): the call must not be refused.
6. The identical sequence with CreateComputeNodeGroup (create, delete the group, re-create with the same clientToken) reproduces it there.
