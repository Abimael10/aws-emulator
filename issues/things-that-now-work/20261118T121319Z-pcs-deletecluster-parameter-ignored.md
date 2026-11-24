Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: ee24c053d2f664e1aece2bc382274e6b27f60b27

# DeleteCluster drops clientToken: a retry with the same idempotency token is refused instead of replaying the original success

_Filed: 2026-11-18T12:13:19Z_
_Operation: DeleteCluster | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `ee24c053d2f664e1aece2bc382274e6b27f60b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteClusterRequest$clientToken` is an `@idempotencyToken` whose documentation states that "the subsequent retries with the same client token return the result from the original successful request and they have no additional effect". The handler never reads `input.client_token` at all: the token is neither recorded nor consulted. A client that retries the identical DeleteCluster call (same token, e.g. after a dropped response) is answered ConflictException ("A cluster isn't in ACTIVE status.", while the cluster is still DELETING) or ResourceNotFoundException (once the settle pass has removed it), instead of the empty success the first call produced.

The crate already owns the replay mechanism and every create uses it (`ComAmazonawsPcsAwsparallelcomputingserviceState::remembered_create` / `remember_create`, keyed `<operation>:<clientToken>`); the delete path simply does not call it. The same omission is in DeleteQueue and DeleteComputeNodeGroup (identical empty-output shape) and in UpdateCluster — none of those four handlers mentions `client_token`. Note that the fix cannot regress the plain double-delete refusal that other coverage relies on: the SDK generates a FRESH token per call ("If you don't specify a client token, the CLI and SDK automatically generate 1 for you"), so only a retry presenting the SAME token replays.

## Evidence

Smithy (smithy_query doc, DeleteClusterRequest$clientToken): "<p>A unique, case-sensitive identifier that you provide to ensure the idempotency of the request. Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect. ...</p>" (the operation is also `@idempotent`; `clientToken` carries `@idempotencyToken`, length 8..100).

The handler never reads the member — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/delete_cluster.rs:172-205 is the whole body after the state lookup, and `grep -n client_token src/ops/*.rs` reports NO hit in delete_cluster.rs, delete_queue.rs, delete_compute_node_group.rs or update_cluster.rs, while the input type does carry it (src/ops/types/delete_cluster_types.rs:18: `pub client_token: Option<String>,`).

The mechanism the siblings use — src/ops/create_cluster.rs:300-311:
```
        // An `@idempotencyToken` retry returns the first call's result and has no
        // further effect, rather than creating a second cluster or conflicting on the
        // name that call took. ...
        let remembered = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
```
and src/ops/create_cluster.rs:361: `state.remember_create(Self::NAME, input.client_token.as_ref(), None, &id);` (also create_queue.rs:287/400 and create_compute_node_group.rs:371/487). The index itself is src/state.rs:742-767 (`remember_create`: "Remember what `operation` with `token` created, so a retry replays it.").

What the retry gets today instead: delete_cluster.rs:193-198 answers `cluster_not_active` while the record is still DELETING, and delete_cluster.rs:175-180 answers `not_found` once `settle` has removed it (src/state.rs:789-803).

Test coverage: `grep -rn clientToken tests/` matches only CREATE calls (tests/all/cluster_lifecycle.rs:793, 806, 819, 833, 911, 918) — no test sends a clientToken on any delete, so no test pins the current behaviour.

## Steps to Reproduce

1. CreateCluster(clusterName="tok", ...) -> 200.
2. GetCluster(clusterIdentifier="tok") -> 200 ACTIVE.
3. DeleteCluster(clusterIdentifier="tok", clientToken="delete-token-0001") -> 200 (cluster DELETING).
4. Repeat the byte-identical request: DeleteCluster(clusterIdentifier="tok", clientToken="delete-token-0001").
5. Expected per the clientToken documentation: the retry returns the original successful (empty) result with no additional effect. Observed: a client error — ResourceNotFoundException ("AWS PCS couldn't find the cluster you specified.") because the settle pass removed the record, or ConflictException ("A cluster isn't in ACTIVE status.") if the record is still DELETING.
6. The same sequence with DeleteQueue / DeleteComputeNodeGroup reproduces it for those operations.
