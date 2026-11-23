Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 7012ff15aecebd44e821216c3792d3327cf3e3b1

# DeleteQueue drops clientToken: a retry with the same idempotency token is refused ResourceNotFoundException instead of replaying the original success

_Filed: 2026-11-20T23:51:31Z_
_Operation: DeleteQueue | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `7012ff15aecebd44e821216c3792d3327cf3e3b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteQueueRequest$clientToken` is an `@idempotencyToken` on an `@idempotent` operation, and its documentation states that retries with the same client token "return the result from the original successful request and they have no additional effect". The handler never reads `input.client_token`: the token is neither remembered nor consulted.

A client that re-sends the byte-identical DeleteQueue call (same token, e.g. after a dropped response) therefore gets a client error rather than the empty success the first call produced: the delete owes `PendingChange::Remove`, the next request's settle pass drops the record, and the retry falls into the queue lookup at delete_queue.rs:213-221 and is answered `ResourceNotFoundException`.

The crate already owns the replay mechanism and several siblings use it — including an UPDATE path, so this is not a create-only facility: `UpdateComputeNodeGroup` records and replays the same token through `ComAmazonawsPcsAwsparallelcomputingserviceState::remembered_create` / `remember_create`, keyed `<operation>:<clientToken>`. The delete path simply does not call it. (The same omission on DeleteCluster is already filed; this is the queue operation, whose retry window differs — a queue goes straight from DELETING to gone.)

A fix cannot regress the plain double-delete refusal that tests/all/delete_ordering.rs relies on: that test re-sends the delete with NO clientToken, and the SDK generates a FRESH token per call ("If you don't specify a client token, the CLI and SDK automatically generate 1 for you"), so only a retry presenting the SAME token replays.

## Evidence

Smithy (smithy_query query="doc" shape="DeleteQueueRequest" member="clientToken"): "<p>A unique, case-sensitive identifier that you provide to ensure the idempotency of the request. Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect. If you don't specify a client token, the CLI and SDK automatically generate 1 for you.</p>" (OPERATIONS.md:139-140 also records `**Traits:** @idempotent` and `clientToken` (string, len 8..100, @idempotencyToken)).

The member exists on the input type but is never read — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/types/delete_queue_types.rs:19: `pub client_token: Option<String>,` while `grep -n client_token src/ops/delete_queue.rs` reports NO hit; the whole handler body is src/ops/delete_queue.rs:166-246.

What the retry gets instead — src/ops/delete_queue.rs:213-221:
```
        let Some(queue_id) = cluster
            .queue(&input.queue_identifier)
            .map(|queue| queue.id.clone())
        else {
            return Err(DeleteQueueErrorModeled::ResourceNotFoundException(
                crate::state::not_found(crate::state::QUEUE, &input.queue_identifier),
            )
            .into());
        };
```
because the first call left `pending = Some(PendingChange::Remove)` (src/ops/delete_queue.rs:240-244) which the next request's settle materialises (src/state.rs:520 `self.queues.retain(|_, queue| !queue.settle());`, src/state.rs:936 `Some(PendingChange::Remove) => true`).

The sibling that DOES replay the same token on a non-create path — src/ops/update_compute_node_group.rs:349-356:
```
        // An `@idempotencyToken` retry returns the first call's result and has NO further
        // effect, so the stored record is answered without applying the change a second
        // time. ...
        if let Some(remembered) = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
            .cloned()
```
and the create sibling src/ops/create_queue.rs:286-288 / 401-406 (`state.remember_create(Self::NAME, input.client_token.as_ref(), Some(&cluster_id), &id);`). The index itself is src/state.rs:806-831 ("Remember what `operation` with `token` created, so a retry replays it.").

Test coverage: `grep -rn clientToken tests/` matches only creates and the UpdateComputeNodeGroup replay test (tests/all/cluster_lifecycle.rs:793, 806, 819, 833, 911, 918; tests/all/cluster_update_and_endpoints/compute_node_group_update.rs:777, 796, 821, 855). tests/all/delete_ordering.rs:343-350 pins the TOKENLESS double delete ("Deleting it again names a queue the cluster no longer holds" -> ResourceNotFoundException), so no test pins the token case.

## Steps to Reproduce

1. CreateCluster(clusterName="tok-cluster", ...) -> 200; the next request settles it ACTIVE.
2. CreateQueue(clusterIdentifier="tok-cluster", queueName="work", ...) -> 200; the next request settles it ACTIVE.
3. DeleteQueue(clusterIdentifier="tok-cluster", queueIdentifier="work", clientToken="queue-delete-token-0001") -> 200 (queue DELETING).
4. Re-send the byte-identical request: DeleteQueue(clusterIdentifier="tok-cluster", queueIdentifier="work", clientToken="queue-delete-token-0001").
5. Expected per the clientToken documentation: the retry returns the original successful (empty) result and has no additional effect. Observed: HTTP 400 ResourceNotFoundException ("AWS PCS couldn't find the queue you specified."), because the settle pass removed the record and the token was never remembered.
