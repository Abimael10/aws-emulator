Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: d1b4a778e7e158b48ec61c845affa7d1f6ee20bd

# The control-plane registration seam breaks the two association invariants its sibling seam states: it records role ids for a refused registration and admits duplicate (namespace, service account) pairs

_Filed: 2026-11-24T15:39:48Z_
_Operation: AssumeRoleForPodIdentity | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d1b4a778e7e158b48ec61c845affa7d1f6ee20bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ComAmazonawsEksauthEksauthfrontendState::program_pod_identity_associations` is documented as the same registration seam as `associate_pod_identity`, with exactly one stated difference ("the association id and ARN are the ones the control plane published, not ones minted here"). It nevertheless breaks two state invariants that the sibling seam states and enforces, and both are what `AssumeRoleForPodIdentity` then resolves an exchange against:

1. **A refused registration records a role id.** `associate_pod_identity` documents "Nothing is recorded for a refused registration — not even the role's id", and its code mints the role id only after all three refusal checks pass, with a comment spelling out why. `program_pod_identity_associations` does the opposite: it mints and records a role id for every eligible entry BEFORE it tries to register the cluster, and then bails out with `return 0` when `register_cluster` refuses the name — so a programming attempt that programs nothing has still written permanent entries into the shared `role_ids` map (`role_id` inserts via `entry(..).or_insert_with(..)`). The EKS control plane really does reach this refusal path: `services/service-com-amazonaws-eks-awswesleyfrontend/src/state/pod_identity_plane.rs:68-78` logs "the EKS Auth plane holds no cluster of this name" for exactly the names `register_cluster` refuses, and `program_cluster_associations` passes the same unvalidated cluster name through at line 116.

2. **Duplicate (namespace, service account) pairs are admitted.** `Cluster::association_for` documents "A pod identity association is unique per (namespace, service account) pair within a cluster, so at most one can match", and `upsert_association` enforces that by re-pointing an existing pair instead of pushing a second record. `program_pod_identity_associations` assigns `cluster.associations` straight from the programmed slice with no uniqueness check at all, so two entries naming the same pair both land in the vector; `association_for`'s `find` then silently resolves the exchange to whichever happens to come first in input order, and the `podIdentityAssociation` / `assumedRoleUser` / `credentials` the operation answers with come from that arbitrary record.

The fix is to make this seam uphold the same two invariants: resolve/mint role ids only once the registration is going to be recorded, and collapse entries by (namespace, service account) the way `upsert_association` does.

## Evidence

Side A — the rule the sibling seam states and enforces, `services/service-com-amazonaws-eksauth-eksauthfrontend/src/state.rs:267-270`:
```
    /// Refused with the reason the EKS control plane refuses on: an unregistered cluster,
    /// a `roleArn` that is not an IAM role ARN, or a namespace or service account name
    /// Kubernetes would not accept. Nothing is recorded for a refused registration — not
    /// even the role's id.
```
and `state.rs:290-293`:
```
        // The role's id is state the whole service shares, so it is resolved — and minted
        // if this is the first association of the role — only once the registration is
        // going to be recorded.
        let role_id = self.role_id(role_arn);
```
(`role_id` is the permanent write — `state.rs:368-372`: `self.role_ids.entry(role_arn.to_string()).or_insert_with(|| unique_id("AROA")).clone()`.)

Side B — the same seam's twin breaking it, `state.rs:330-338`:
```
        // The role ids are state the whole service shares, so they are resolved — and
        // minted for a role this service has not seen — before the cluster is borrowed.
        let role_ids: Vec<String> = eligible
            .iter()
            .map(|association| self.role_id(&association.role_arn))
            .collect();
        let Some(cluster) = self.register_cluster(cluster_name, region) else {
            return 0;
        };
```
Its own doc claims the only difference from the sibling is the published ids — `state.rs:305-309`: "This is the registration seam [`Self::associate_pod_identity`] is for a caller that has no control plane behind it, with the difference that matters for what `AssumeRoleForPodIdentity` REPORTS: the association id and ARN are the ones the control plane published, not ones minted here".

Second invariant, side A — `state.rs:505-507`:
```
    /// A pod identity association is unique per (namespace, service account) pair within
    /// a cluster, so at most one can match.
```
enforced at `state.rs:464-475` (`upsert_association` finds the existing pair by `a.namespace == namespace && a.service_account == service_account` and re-points it rather than pushing).

Second invariant, side B — `state.rs:339-354`: `cluster.associations = eligible.iter().zip(role_ids).map(|(association, role_id)| PodIdentityAssociationRecord { .. }).collect();` — no pair check anywhere in the function, while `association_for` (`state.rs:513-515`) resolves with `.find(|a| a.namespace == namespace && a.service_account == service_account)`.

Reachability of the refusal path from the control plane — `services/service-com-amazonaws-eks-awswesleyfrontend/src/state/pod_identity_plane.rs:68-78`:
```
pub fn register_cluster(plane: &AuthPlane, region: &str, name: &str) {
    if plane.lock().register_cluster(name, region).is_none() {
        // A name EKS Auth itself refuses can reach no request there, so nothing is
        // registered for it; the cluster is still this service's.
```
and `pod_identity_plane.rs:114-116`: `plane.lock().program_pod_identity_associations(&key.1, &key.0, &programmed)` — the same unchecked cluster name.

Test coverage: the invariant is pinned for the sibling only — `services/service-com-amazonaws-eksauth-eksauthfrontend/tests/all/assume_role_for_pod_identity.rs:837-840`:
```
        assert_eq!(
            role_ids_after, role_ids_before,
            "case {name:?}: a refused registration must record no role id"
        );
```
over rows that include `("unregistered cluster", ..)` (line 752-758). No test calls `program_pod_identity_associations` at all (grep for it in this crate matches only `src/state.rs` and the EKS crate), so neither invariant is covered for that seam.

## Steps to Reproduce

1. Build the eksauth crate's test harness (`tests/all/assume_role_for_pod_identity.rs::Harness::seeded`) and take `let state = service_state(&*harness.engine)`.
2. Snapshot the shared role-id map: `let before: Vec<String> = state.lock().role_ids.keys().cloned().collect();`.
3. Call the control-plane seam with a cluster name `register_cluster` refuses (the same rows the sibling test uses, e.g. `"cluster.one"` or `""`) and one eligible association naming a role never seen before: `state.lock().program_pod_identity_associations("cluster.one", DEFAULT_REGION, &[ProgrammedAssociation { association_id: "a-00000000000000000".into(), association_arn: "arn:aws:eks:us-east-1:000000000000:podidentityassociation/cluster.one/a-00000000000000000".into(), namespace: "default".into(), service_account: "my-sa".into(), role_arn: "arn:aws:iam::000000000000:role/brand-new-role".into(), policy: None, target_role_arn: None, external_id: None, disable_session_tags: None }])`.
4. Observe the return value is 0 (nothing programmed, no cluster registered) but `state.lock().role_ids` now contains a freshly minted `AROA…` entry for `arn:aws:iam::000000000000:role/brand-new-role` — the sibling seam's documented rule ("Nothing is recorded for a refused registration — not even the role's id") is broken.
5. For the second symptom: program a REGISTERED cluster (e.g. `"demo-cluster"`) with TWO `ProgrammedAssociation` entries carrying the same `namespace`/`service_account` (`default`/`my-sa`) but different `association_id` and `role_arn`.
6. Observe the return value is 2 and `guard.cluster("demo-cluster").unwrap().associations` holds both records for the same pair, contradicting `association_for`'s "at most one can match"; then exchange a token for that service account over `POST /clusters/demo-cluster/assume-role-for-pod-identity` and observe `podIdentityAssociation` / `assumedRoleUser` come from whichever record was first in the programmed slice.
