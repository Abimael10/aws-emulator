Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 4a35b4279082e5710ac65a955f83cb79cbeb610c

# CreateCluster's idempotency replay ignores the clusterName the request named, so a reused clientToken silently answers with a different cluster and creates nothing

_Filed: 2026-11-22T19:05:40Z_
_Operation: CreateCluster | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `4a35b4279082e5710ac65a955f83cb79cbeb610c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

On the `@idempotencyToken` replay path, `CreateCluster` looks the remembered token up and returns the FIRST call's cluster without comparing anything else the request carried — in particular the `@required` `clusterName`. A second `CreateCluster` that reuses a token but names a different cluster is answered HTTP 200 with the OTHER cluster's `name`/`id`/`arn`, nothing is persisted for the name the caller asked for, and the caller is given no refusal that would tell it so. A client that reuses one token across two creates (a fixed/derived `clientToken` in provisioning code is the common way this happens) therefore believes it created `beta` and holds a response describing `alpha`; `GetCluster("beta")`/`ListClusters` never report it.

This is not how the crate's own sibling creates behave. `CreateQueue` and `CreateComputeNodeGroup` compare the remembered create against the resource the retry ADDRESSES and refuse the mismatch with `ConflictException` rather than replaying someone else's resource — a rule the crate states in a comment, implements in both handlers, and pins with a test. `CreateCluster` is the only create of the three that replays a remembered token without checking that the retry names the same resource, and it is the only one for which the mismatch is unobservable.

(The fall-through case is handled: when the remembered cluster has since been deleted the lookup misses and a fresh create runs. What is missing is only the parameter comparison the siblings make.)

## Evidence

The replay, with no comparison against `input.cluster_name` — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_cluster.rs:300-311:
```
        // An `@idempotencyToken` retry returns the first call's result and has no
        // further effect, rather than creating a second cluster or conflicting on the
        // name that call took. The remembered entry is never overwritten, so the token
        // names the FIRST call's cluster for as long as it is remembered.
        let remembered = state
            .remembered_create(Self::NAME, input.client_token.as_ref())
            .map(|remembered| remembered.resource_id.clone());
        if let Some(first) = remembered.as_deref().and_then(|id| state.clusters.get(id)) {
            return Ok(CreateClusterOutput {
                cluster: Some(first.to_api()),
            });
        }
```
The name the request carried is never read again on this path: the name-conflict check and the insert below it (create_cluster.rs:316-360) are both after the `return`.

The sibling rule, stated and enforced — src/ops/create_queue.rs:291-317:
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
                .then(|| { ... })
                .flatten();
            let Some(first) = first else {
                return Err(CreateQueueErrorModeled::ConflictException(ConflictException {
                    message: format!(
                        "clientToken was already used to create queue {}",
                        remembered.resource_id
                    ),
```
and the same arm in src/ops/create_compute_node_group.rs:368-396 ("so a retry naming a DIFFERENT cluster cannot slip past the replay and create a second group: it conflicts").

Test coverage: the sibling rule is pinned — tests/all/cluster_lifecycle.rs:900 `an_idempotency_token_cannot_be_replayed_into_another_cluster`, with "The token names the first call's queue, in the cluster that call created it in. Replaying it against another cluster must not create a second queue." (cluster_lifecycle.rs:935-943) and an assertion that no queue was written elsewhere (lines 944-956). The CreateCluster rows of tests/all/cluster_lifecycle.rs:791-817 only replay the SAME request (`first_cluster` reused verbatim, line 803), so no test pins what a token replayed under a different `clusterName` answers.

Smithy — `CreateClusterRequest$clusterName` is `@required`: "A name to identify the cluster. Example: MyCluster", and `CreateCluster`'s `ConflictException` documents "A cluster with the same name already exists." — the name is a value the operation decides on, not one it may drop.

## Steps to Reproduce

1. CreateCluster(clusterName="alpha", scheduler={type:"SLURM",version:"24.11"}, size="SMALL", networking={subnetIds:[<a resolvable subnet>]}, clientToken="token-0001") -> 200, cluster.name="alpha".
2. CreateCluster(clusterName="beta", same scheduler/size/networking, clientToken="token-0001") — same token, different name.
3. Observed: HTTP 200 whose body is cluster.name="alpha" (the first call's id and ARN). Nothing about "beta" is persisted and no refusal is returned.
4. GetCluster(clusterIdentifier="beta") -> ResourceNotFoundException; ListClusters reports one cluster, "alpha".
5. Compare the sibling path: CreateQueue(clusterIdentifier=<cluster A>, queueName="work", clientToken="t") then CreateQueue(clusterIdentifier=<cluster B>, queueName="work", clientToken="t") -> ConflictException ("clientToken was already used to create queue ..."), as tests/all/cluster_lifecycle.rs:900 pins.
