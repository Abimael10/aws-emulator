Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 79023c4f507908bc4e0b94b04ba9cd6b317395a7

# UpdateCluster applies a scheduler version OLDER than the cluster's, although the member documents "You can only update to a newer version"

_Filed: 2026-11-19T03:08:30Z_
_Operation: UpdateCluster | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `79023c4f507908bc4e0b94b04ba9cd6b317395a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateClusterRequest$scheduler` is an `UpdateSchedulerRequest` whose single (`@required`) member `version` is documented as "The scheduler version to update the cluster to. You can only update to a newer version." The handler never compares the requested version with the one the cluster is already on: the only gate on the path is `crate::state::update_scheduler_version_violation`, which is a pure SHAPE check (two or three dot-separated components, each 1..4 ASCII digits — `names_a_scheduler_release`), and the apply step then assigns the value unconditionally over the stored one.

Result: a request asking to move a cluster from `26.05` back to `23.11` is accepted with HTTP 200, the cluster is moved to `UPDATING`, and `GetCluster` afterwards reports the OLDER version as the cluster's scheduler version. Nothing in the crate refuses the downgrade, even though the refusal channel for a scheduler version this path cannot take already exists (the modelled `ValidationException` with the service's fixed sentence "AWS PCS doesn't support the scheduler version you provided."), and the value to compare against is already in hand at the point of assignment (`ClusterRecord::scheduler`, the record the handler is holding mutably).

Scope note (to keep this from being read as a re-litigation of settled ground): this is NOT about the printed "Valid Values" set. The crate deliberately refuses to gate on that set (state.rs:1199-1206, "the check is a SHAPE check and nothing more, never the set of releases the service can provision"), and that decision came out of a fixed issue; it must stay. The rule missing here is the ORDERING rule the model states in the member's own documentation, which no comment, doc or rationale in the crate addresses.

Also distinct from the open UpdateCluster issue 20261118T121319Z-pcs-updatecluster-validation (children-must-be-ACTIVE precondition): different precondition, different member, different code path.

## Evidence

Smithy (smithy_query query="doc", shape="UpdateSchedulerRequest", member="version"), verbatim: "<p>The scheduler version to update the cluster to. You can only update to a newer version. For more information about supported versions and update paths, see <a href=\"https://docs.aws.amazon.com/pcs/latest/userguide/working-with_clusters_version_update.html\">Updating the scheduler version on a cluster</a> in the <i>PCS User Guide</i>.</p> <p>Valid Values: <code>24.05 | 24.11 | 25.05 | 25.11 | 26.05</code> </p>"

The only version gate on the path — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/update_cluster.rs:191-197:
```
        if let Some(scheduler) = input.scheduler.as_ref() {
            if let Some(refusal) =
                crate::state::update_scheduler_version_violation(&scheduler.version)
            {
                return Err(UpdateClusterErrorModeled::ValidationException(refusal).into());
            }
        }
```
and what that gate decides — src/state.rs:1230-1249:
```
pub fn update_scheduler_version_violation(version: &str) -> Option<ValidationException> {
    if names_a_scheduler_release(version) {
        return None;
    }
...
fn names_a_scheduler_release(version: &str) -> bool {
    let components: Vec<&str> = version.split('.').collect();
    SCHEDULER_VERSION_COMPONENTS.contains(&components.len())
        && components.iter().all(|component| {
            (1..=SCHEDULER_VERSION_COMPONENT_DIGITS).contains(&component.len())
                && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}
```
The unconditional overwrite, with the stored version right there — src/ops/update_cluster.rs:245-264:
```
        // A member the request carries is applied; one it omits leaves the stored value
        // alone. ...
        let mut changed = false;
        if let Some(scheduler) = input.scheduler {
            cluster.scheduler.version = scheduler.version;
            changed = true;
        }
```
(`cluster` is the mutable `ClusterRecord` from update_cluster.rs:229, and src/state.rs:253-254 is `/// The scheduler the request asked for.` `pub scheduler: Scheduler,` — so `cluster.scheduler.version` holds the version being replaced; no new state is needed to decide the rule.)

Test coverage, disclosed in full: tests/all/cluster_update_and_endpoints.rs:21-23 lists `const RELEASE_LABELS: &[&str] = &["24.11", "25.05", "25.11", "26.05", "23.11", "25.05.1", "25.5", "9999.1"];` and `update_cluster_accepts_every_scheduler_release_label` (line 294) walks them IN THAT ORDER, asserting each one is applied: lines 311-342 assert `status == 200`, `cluster.status == "UPDATING"` and, after `GetCluster`, `settled["cluster"]["scheduler"]["version"] == version`. The `23.11` row therefore follows the `26.05` row and pins today's acceptance of a DOWNGRADE. The stated reason for that list is only about the printed value set — lines 14-20: "Values that LABEL a scheduler release ... because the member is a plain `String` with no enum, `@pattern` or `@length`, so the printed list is prose rather than a wire constraint" — and the loop comment at 309-310 claims "every release label is applied", which is exactly what the quoted model sentence contradicts. Closing this means re-ordering those rows (ascending) and adding a descending row that expects the refusal; it does not reinstate any set-membership gate.

## Steps to Reproduce

1. CreateCluster(clusterName="ver-order", scheduler={type:"SLURM", version:"26.05"}, size="SMALL", networking={subnetIds:[<a subnet EC2 holds>]}) -> 200.
2. GetCluster(clusterIdentifier="ver-order") -> 200, cluster.status ACTIVE, cluster.scheduler.version == "26.05".
3. UpdateCluster(clusterIdentifier="ver-order", scheduler={version:"23.11"}).
4. Expected per `UpdateSchedulerRequest$version` ("You can only update to a newer version"): the request is refused with the modelled ValidationException (the path's own sentence "AWS PCS doesn't support the scheduler version you provided.") and the stored version is left at 26.05.
5. Observed: HTTP 200, cluster.status "UPDATING", cluster.scheduler.version "23.11"; GetCluster on the next request reports ACTIVE with scheduler.version "23.11" — the cluster has been silently downgraded.
6. The same call with an equal version ("26.05" over "26.05") is likewise accepted and churns the cluster to UPDATING.
