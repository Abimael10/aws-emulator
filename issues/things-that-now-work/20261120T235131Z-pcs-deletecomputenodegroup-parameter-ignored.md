Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 7012ff15aecebd44e821216c3792d3327cf3e3b1

# DeleteComputeNodeGroup drops clientToken: a retry with the same idempotency token is refused instead of replaying the original success

_Filed: 2026-11-20T23:51:31Z_
_Operation: DeleteComputeNodeGroup | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `7012ff15aecebd44e821216c3792d3327cf3e3b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteComputeNodeGroupRequest$clientToken` is an `@idempotencyToken` on an `@idempotent` operation, and its documentation states that retries with the same client token "return the result from the original successful request and they have no additional effect". The handler never reads `input.client_token`: the token is neither remembered nor consulted.

So a client that re-sends the byte-identical DeleteComputeNodeGroup call (same token, e.g. after a dropped response) is answered with a client error instead of the empty success the first call produced:

* on the request immediately after the accepted delete, the settle pass has moved the group to `DELETED` (`PendingChange::MarkDeleted`), so the status gate at delete_compute_node_group.rs:249-254 answers `ConflictException` ("A compute node group isn't in ACTIVE status.");
* one request later the record is gone, so delete_compute_node_group.rs:219-225 answers `ResourceNotFoundException`.

The crate already owns the replay mechanism and several siblings use it — including an UPDATE path, so this is not a create-only facility: `UpdateComputeNodeGroup` records and replays the same token through `ComAmazonawsPcsAwsparallelcomputingserviceState::remembered_create` / `remember_create`, keyed `<operation>:<clientToken>`. The delete path simply does not call it. (The same omission on DeleteCluster is already filed; this issue is the compute-node-group operation, which carries the extra `DELETED` step and therefore the extra ConflictException window.)

A fix cannot regress the plain double-delete refusal that the delete-ordering coverage relies on: the SDK generates a FRESH token per call ("If you don't specify a client token, the CLI and SDK automatically generate 1 for you"), so only a retry presenting the SAME token replays.

## Evidence

Smithy (smithy_query query="doc" shape="DeleteComputeNodeGroupRequest" member="clientToken"): "<p>A unique, case-sensitive identifier that you provide to ensure the idempotency of the request. Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect. If you don't specify a client token, the CLI and SDK automatically generate 1 for you.</p>" (OPERATIONS.md:104-105 also records `**Traits:** @idempotent` and `clientToken` (string, len 8..100, @idempotencyToken) for this operation).

The member exists on the input type but is never read — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/types/delete_compute_node_group_types.rs:19: `pub client_token: Option<String>,` while `grep -n client_token src/ops/delete_compute_node_group.rs` reports NO hit; the whole handler body is src/ops/delete_compute_node_group.rs:166-263.

What the retry gets instead — src/ops/delete_compute_node_group.rs:247-254:
```
        // Only a settled group can be deleted: one still provisioning, or already on its
        // way out, has an operation of its own in flight.
        if group.status != crate::state::STATUS_ACTIVE {
            return Err(DeleteComputeNodeGroupErrorModeled::ConflictException(
                crate::state::not_active(crate::state::COMPUTE_NODE_GROUP, &group.id),
            )
            .into());
        }
```
and src/ops/delete_compute_node_group.rs:219-225 (`crate::state::not_found(crate::state::COMPUTE_NODE_GROUP, ...)`) once the record has been forgotten (src/state.rs:930-936: `Some(PendingChange::MarkDeleted) => { *status = STATUS_DELETED...; *pending = Some(PendingChange::Remove); }`, then `Some(PendingChange::Remove) => true`).

The sibling that DOES replay the same token on a non-create path — src/ops/update_compute_node_group.rs:349-356:
```
        // An `@idempotencyToken` retry returns the first call's result and has NO further
        // effect, so the stored record is answered without applying the change a second
        // time. ...
        if let Some(remembered) = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
            .cloned()
```
and src/ops/update_compute_node_group.rs:467-472 (`state.remember_create(Self::NAME, input.client_token.as_ref(), Some(&cluster_id), &group_id);`). The index itself is src/state.rs:806-831 ("Remember what `operation` with `token` created, so a retry replays it.").

Test coverage: `grep -rn clientToken tests/` matches only creates and the UpdateComputeNodeGroup replay test (tests/all/cluster_lifecycle.rs:793, 806, 819, 833, 911, 918; tests/all/cluster_update_and_endpoints/compute_node_group_update.rs:777, 796, 821, 855). No test sends a clientToken on any delete; tests/all/delete_ordering.rs:343-350 re-sends a delete WITHOUT a token ("Deleting it again names a queue the cluster no longer holds"), so nothing pins the token case.

## Steps to Reproduce

1. CreateCluster(clusterName="tok-cluster", ...) -> 200; the next request settles it ACTIVE.
2. CreateComputeNodeGroup(clusterIdentifier="tok-cluster", computeNodeGroupName="nodes", ...) -> 200; the next request settles it ACTIVE.
3. DeleteComputeNodeGroup(clusterIdentifier="tok-cluster", computeNodeGroupIdentifier="nodes", clientToken="cng-delete-token-0001") -> 200 (group DELETING).
4. Re-send the byte-identical request: DeleteComputeNodeGroup(clusterIdentifier="tok-cluster", computeNodeGroupIdentifier="nodes", clientToken="cng-delete-token-0001").
5. Expected per the clientToken documentation: the retry returns the original successful (empty) result and has no additional effect. Observed: a client error — ConflictException "A compute node group isn't in ACTIVE status." while the record is DELETED, or ResourceNotFoundException "AWS PCS couldn't find the compute node group you specified." once the record is forgotten.
