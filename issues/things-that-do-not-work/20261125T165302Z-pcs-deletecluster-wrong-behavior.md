Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ff726bd67edf765e79ac63fc928612486072107b

# A remembered DeleteCluster clientToken is matched by cluster NAME, so deleting a recreated cluster of the same name is answered 200 (or 409) without deleting anything

_Filed: 2026-11-25T16:53:02Z_
_Operation: DeleteCluster | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ff726bd67edf765e79ac63fc928612486072107b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

DeleteCluster consults its remembered idempotency token BEFORE it looks the cluster up, and the remembered entry is matched against the request's `clusterIdentifier` by resource id OR by the resource NAME the first call deleted (`RememberedCreate::names`). Unlike a remembered CREATE, a remembered DELETE deliberately outlives the record it points at: `forget_creates_for_cluster` keeps it when the cluster is removed.

A cluster NAME is reusable, though: `settle` drops the removed cluster's `cluster_ids_by_name` entry and `CreateCluster` conflicts only on a name a STORED cluster holds, so after a delete settles a brand-new cluster can be created under the same name with a different generated id. From then on the stale delete entry matches by name, and a genuine delete of the NEW cluster is answered as a replay of the OLD one:

* addressed by name (`clusterIdentifier: "A"`) -> `RememberedDelete::Replay` -> HTTP 200 with an empty success body, and the new cluster is never marked DELETING and never removed. The caller is told the delete succeeded while the cluster is still ACTIVE and still visible to GetCluster/ListClusters.
* addressed by the new cluster's generated id -> neither `resource_id` nor `resource_name` matches -> `RememberedDelete::Conflict` -> ConflictException "clientToken was already used to delete cluster <old id>", so the new cluster is not deleted on that path either.

The crate's own state doc says the name is kept beside the id precisely "so a retry can be told apart from a request that merely reuses the token" — but matching on the name cannot tell a retry apart from a delete of a DIFFERENT cluster that merely reuses the name, which is exactly what a client with a deterministic token (derived from the cluster name rather than generated per call, as provisioning tools do) sends on every delete/recreate/delete cycle.

The identical shared helper is reached the same way by `DeleteQueue` (delete_queue.rs:226-231) and `DeleteComputeNodeGroup` (delete_compute_node_group.rs:230-235), whose `queueIdentifier`/`computeNodeGroupIdentifier` names are reusable inside a cluster for the same reason; this is filed on DeleteCluster as the first handler that calls it.

## Evidence

The handler consults the token before the lookup and hands it the identifier as received — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/delete_cluster.rs:181-194:
```
        // An `@idempotencyToken` retry returns the first call's result and has NO further
        // effect, so a delete that already used this token is replayed with its empty
        // success — BEFORE the cluster is looked up, because the cluster the first call
        // took out is exactly what a retry can no longer find, ...
        match state.remembered_delete(
            Self::NAME,
            input.client_token.as_ref(),
            None,
            &input.cluster_identifier,
        ) {
            Some(crate::state::RememberedDelete::Replay) => return Ok(DeleteClusterOutput {}),
```

The match is by id OR name — src/state.rs:940-946:
```
        let remembered = self.remembered_create(operation, token)?;
        if remembered.cluster_id.as_deref() == cluster_id && remembered.names(identifier) {
            return Some(RememberedDelete::Replay);
        }
        Some(RememberedDelete::Conflict(remembered.clone()))
```
src/state.rs:703-708:
```
impl RememberedCreate {
    /// Whether `identifier` names the resource this entry was remembered for — by the
    /// generated id, or by the name a remembered delete also kept.
    fn names(&self, identifier: &str) -> bool {
        self.resource_id == identifier || self.resource_name.as_deref() == Some(identifier)
    }
```

The rule the name is supposed to serve — src/state.rs:689-694:
```
    /// The name that resource was known by, so a retry can be told apart from a request
    /// that merely reuses the token: a CREATE mints its resource, so the name its
    /// `@required` `*Name` member asks for is what says whether the retry is the first
    /// call again, and a DELETE outlives its record, so the name is the other form its
    /// `*Identifier` may address the resource by once there is nothing left to resolve it
    /// against.
```

Why the stale entry survives the record — src/state.rs:965-986 (`forget_creates_for_cluster`): "What stays is the DELETE of the cluster itself ([`RememberedCreate::deleted`]): that entry exists precisely for the retry that arrives once the record is gone" with `remembered.resource_id != cluster_id || remembered.deleted`.

Why the NAME is reusable — src/state.rs:1000-1007 (`settle`) removes the name index with the record:
```
            if let Some(cluster) = self.clusters.remove(&id) {
                if self.cluster_ids_by_name.get(&cluster.name) == Some(&cluster.id) {
                    self.cluster_ids_by_name.remove(&cluster.name);
                }
            }
```
and src/ops/create_cluster.rs:350-357 conflicts only on a name a stored cluster still holds:
```
        if let Some(existing) = state.cluster_ids_by_name.get(&input.cluster_name) {
            return Err(CreateClusterErrorModeled::ConflictException(ConflictException {
                message: "A cluster with the name you specified already exists.".to_string(),
```

Model (smithy_query doc, CreateClusterRequest$clientToken, the same `SBClientToken` `@idempotencyToken` DeleteCluster takes): "Idempotency ensures that an API request completes only once. With an idempotent request, if the original request completes successfully, the subsequent retries with the same client token return the result from the original successful request and they have no additional effect." — the second delete here is not a retry of the original request: it addresses a cluster the original call never saw.

Test coverage: tests/all/delete_ordering/idempotent_cluster_delete_retry.rs and tests/all/delete_ordering/idempotent_delete_retry.rs exercise a reused token against the SAME resource only; nothing in the suite recreates a resource under the deleted name and reuses the token, so no test pins the current replay.

## Steps to Reproduce

1. CreateCluster(clusterName="A", scheduler={type:SLURM,version:"24.11"}, size="SMALL", networking={subnetIds:["subnet-0123456789abcdef0"]}) -> 200, cluster pcs_1.
2. Issue any request (e.g. GetCluster("A")) so the create settles and pcs_1 is ACTIVE.
3. DeleteCluster(clusterIdentifier="A", clientToken="tok-A-0001") -> 200; pcs_1 is DELETING.
4. Issue any request so the delete settles: pcs_1 is removed, its name index entry is dropped, and the remembered DeleteCluster token entry (resource_id=pcs_1, resource_name="A", deleted=true) is deliberately kept.
5. CreateCluster(clusterName="A", ...) -> 200, a NEW cluster pcs_2; one more request settles it to ACTIVE.
6. Re-issue the delete the deterministic token belongs to: DeleteCluster(clusterIdentifier="A", clientToken="tok-A-0001").
7. Observed: HTTP 200 with an empty body (RememberedDelete::Replay). GetCluster("A") still answers 200 with pcs_2 ACTIVE — the cluster the caller asked to delete was never marked DELETING and is never removed.
8. Variant: at step 6 address the new cluster by its own id, DeleteCluster(clusterIdentifier="pcs_2", clientToken="tok-A-0001") -> ConflictException "clientToken was already used to delete cluster pcs_1"; pcs_2 is again not deleted.
9. The same sequence reproduces on DeleteQueue and DeleteComputeNodeGroup (create queue "work", delete it, recreate "work", delete with the same clientToken).
