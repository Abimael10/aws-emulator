Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: a4af9501c1d5c983daa3ec1227964a73edd2f5b5

# CreateQueue's idempotency replay ignores the queueName the request named, so a reused clientToken answers 200 with a different queue and creates nothing

_Filed: 2026-11-23T23:16:42Z_
_Operation: CreateQueue | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `a4af9501c1d5c983daa3ec1227964a73edd2f5b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateQueue`'s `@idempotencyToken` replay compares only the CLUSTER the retry names against the remembered entry, then returns the first call's queue. The `@required` `queueName` of the second request is never read again on that path — the duplicate-name conflict check and the insert that would store it are both after the `return`. A second `CreateQueue` in the SAME cluster that reuses a token but names a different queue is answered HTTP 200 with the OTHER queue's `name`/`id`/`arn`; nothing is persisted for the name the caller asked for and no refusal is returned, so the caller cannot tell. `GetQueue` on the requested name answers not-found and `ListQueues` never reports it.

The handler's own comment states the guard it implements — "a retry naming a DIFFERENT cluster cannot slip past the replay and create a second queue: it conflicts" — and the refusal it would use (`ConflictException` "clientToken was already used to create queue …") is already written. A retry naming a different QUEUE inside the same cluster slips past that guard and is reported as a success.

(This is a different defect from the open CreateQueue `wrong-behavior` issue, which is about the opposite arm of the same block: a remembered token whose queue has since been DELETED being refused forever. Here the lookup SUCCEEDS and the wrong resource is reported.)

## Evidence

Smithy, `CreateQueueRequest$clientToken` (smithy_query doc): "Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect." — and `CreateQueueRequest$queueName` is `@required`.

The replay, comparing the cluster only — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:291-321:
```
        // An `@idempotencyToken` retry returns the first call's result and has no
        // further effect. The token is remembered with the cluster the first call
        // created the queue in, so a retry naming a DIFFERENT cluster cannot slip past
        // the replay and create a second queue: it conflicts.
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
...
            return Ok(CreateQueueOutput {
                queue: Some(first.to_api()),
            });
        }
```
`input.queue_name` is read only BELOW that `return`: the duplicate-name conflict at create_queue.rs:341-354 (`.find(|queue| queue.name == input.queue_name)`, message "A queue already exists in a cluster.") and the stored record at create_queue.rs:398 (`name: input.queue_name,`).

The remembered entry keeps no name on the create path — src/state.rs:835-851 `remember_create(... resource_name: None ...)`, documented at src/state.rs:690-695 as recorded "only where the record itself does not outlive the call — a DELETE".

Test coverage: tests/all/cluster_lifecycle.rs:838-847 replays `first_queue` VERBATIM and asserts the first call's id; tests/all/cluster_lifecycle.rs:900+ (`an_idempotency_token_cannot_be_replayed_into_another_cluster`) covers only the ANOTHER-CLUSTER case. No test re-uses a token under a different `queueName` in the same cluster.

## Steps to Reproduce

1. CreateCluster(clusterName="tok-cluster", ...) -> 200; the next request observes it ACTIVE.
2. CreateComputeNodeGroup(clusterIdentifier="tok-cluster", computeNodeGroupName="nodes", ...) -> 200 (so a queue can associate a group).
3. CreateQueue(clusterIdentifier="tok-cluster", queueName="work", computeNodeGroupConfigurations=[{computeNodeGroupId:"nodes"}], clientToken="queue-token-0001") -> 200, queue.name="work", id=pcs_X.
4. CreateQueue(clusterIdentifier="tok-cluster", queueName="batch", same configurations, clientToken="queue-token-0001") — same cluster, same token, DIFFERENT name.
5. Observed: HTTP 200 whose body is queue.name="work" with id pcs_X; nothing for "batch" is persisted and no refusal is returned.
6. GetQueue(clusterIdentifier="tok-cluster", queueIdentifier="batch") -> ResourceNotFoundException; ListQueues reports one queue, "work".
