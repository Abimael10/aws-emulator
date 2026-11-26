Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 7d5f5571d94544e1e0d6279d2f57c51740cc17a4

# CreateComputeNodeGroup reads the cluster out of the store BEFORE settle(), so a cluster whose removal is already due is refused as a subnet/VPC ValidationException instead of the cluster refusal

_Filed: 2026-11-26T12:13:41Z_
_Operation: CreateComputeNodeGroup | Class: error-response_

> _Note: this issue was identified against the code base at commit `7d5f5571d94544e1e0d6279d2f57c51740cc17a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The state module states, twice, that `settle()` is the FIRST act any handler performs on this store — that is what makes the deferred lifecycle exact, and what guarantees a handler never reads a record whose removal a previous request already made due.

`CreateComputeNodeGroup` breaks it: its first act on the store is a bare `state.lock()` that reads the cluster's control-plane `networking.subnetIds` with no `settle()` call, and only AFTER the EC2 placement hop does it take the real guard and settle. `UpdateComputeNodeGroup` repeats the identical pre-settle read.

Because `settle()` is what REMOVES a cluster carrying `PendingChange::Remove` (set by DeleteCluster), the pre-settle read can still find a cluster this request is obliged to have forgotten, and hands its subnets to `ec2_subnets::placement_violation`. The request is then refused `ValidationException` ("The subnets must be in the same VPC as the cluster.") about a cluster the handler's own cluster-resolution branch, ten lines later, would have refused as `ResourceNotFoundException` (a `pcs_` identifier) or `AccessDeniedException` (a cluster name). The caller is told its subnets are in the wrong VPC for a cluster that no longer exists. The update handler's own comment asserts the opposite of what the code does.

## Evidence

CRATE SIDE A — the invariant (src/state.rs:256-261):
```
//! What keeps that exact: every handler calls
//! [`ComAmazonawsPcsAwsparallelcomputingserviceState::settle`] as its first act on this
//! store, before it owes anything of its own, so a change owed during one request is
//! never materialised by that same request
```
and again on `settle` itself (src/state.rs:1084-1086):
```
/// Apply every lifecycle transition this service owes. Called as the FIRST act of
/// every handler on this store, including the reporting ones: it materialises only
/// changes a previous request already owed, ...
```

CRATE SIDE B — the handler's first act on the store is an UNSETTLED read (src/ops/create_compute_node_group.rs:328-343):
```
let cluster_subnet_ids = {
    let held = state.lock();
    held.cluster(&input.cluster_identifier)
        .and_then(|cluster| cluster.networking.subnet_ids.clone())
        .unwrap_or_default()
};
if let Some(refusal) = crate::state::ec2_subnets::placement_violation(
    ctx, &region, &input.subnet_ids, &cluster_subnet_ids,
).await
{
    return Err(CreateComputeNodeGroupErrorModeled::ValidationException(refusal).into());
}
```
only then (src/ops/create_compute_node_group.rs:349-350):
```
let mut state = state.lock();
state.settle();
```
The same pre-settle read is at src/ops/update_compute_node_group.rs:296-301, under a comment that states the rule the code breaks (src/ops/update_compute_node_group.rs:293-295):
```
// the hop; an identifier that resolves to no cluster leaves nothing to compare
// against and is answered as a cluster refusal below.
```

CRATE SIDE C — settle is what makes the stale record disappear, so a pre-settle read sees it (src/ops/delete_cluster.rs:236-238):
```
cluster.status = crate::state::STATUS_DELETING.to_string();
cluster.modified_at = crate::state::now_date_time();
cluster.pending = Some(crate::state::PendingChange::Remove);
```
and (src/state.rs:1089-1096):
```
pub fn settle(&mut self) {
    let removable: Vec<String> = self
        .clusters
        .iter_mut()
        .filter_map(|(id, cluster)| cluster.settle().then(|| id.clone()))
        .collect();
    for id in removable {
        if let Some(cluster) = self.clusters.remove(&id) {
```

TEST COVERAGE — the VPC-mismatch refusal this takes is exercised in tests/all/subnet_placement.rs and tests/all/subnet_placement/cluster_vpc_placement.rs (which mount EC2 with more than one VPC), and the cluster-refusal branch in tests/all/cluster_scoped_refusals.rs and tests/all/cluster_scoped_refusals/absent_generated_cluster_id.rs; no test exercises the two together, i.e. a child create aimed at a cluster whose DeleteCluster has already been accepted.

## Steps to Reproduce

1. Mount EC2 alongside PCS with two VPCs: vpc-1 holding subnet-A and vpc-2 holding subnet-B.
2. CreateCluster "doomed" with networking.subnetIds = [subnet-A]; issue one more request so it settles to ACTIVE.
3. Call DeleteCluster(clusterIdentifier="doomed"). It answers 200 and the record is left DELETING with PendingChange::Remove — the removal is owed from this instant.
4. On the NEXT request call CreateComputeNodeGroup(clusterIdentifier=<the cluster's pcs_ id>, subnetIds=[subnet-B], plus the other required members).
5. Observe ValidationException "The subnets must be in the same VPC as the cluster." — the pre-settle read at create_compute_node_group.rs:328-333 found the cluster awaiting removal and compared vpc-2 against its vpc-1.
6. Expected, per the crate's own settle-first invariant: settle removes the cluster before anything is read, the cluster lookup at create_compute_node_group.rs:360 fails, and the call is refused ResourceNotFoundException for the cluster (or AccessDeniedException when the identifier is a cluster NAME).
7. Repeat steps 4-5 against UpdateComputeNodeGroup with a subnetIds patch to see the identical pre-settle read at update_compute_node_group.rs:296-301.
