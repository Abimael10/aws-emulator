Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: c40ac6f2013e99490edc897db716ef8183e526eb

# The cluster-scoped refusal branch is restated in all ten child handlers instead of being built once in cluster_authorization

_Filed: 2026-11-26T14:51:10Z_
_Operation: CreateComputeNodeGroup | Class: maintainability_

> _Note: this issue was identified against the code base at commit `c40ac6f2013e99490edc897db716ef8183e526eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::cluster_authorization` exists to be the ONE place that decides what a cluster-scoped child operation answers when its `clusterIdentifier` resolves to no cluster, and its own doc says so: "Asked by every cluster-scoped child operation, so none of them can word this differently." But the module only hands back the DISCRIMINANT (`UnresolvedCluster::NotFound` / `::Denied`); the two refusal PAYLOADS — which exception to build, and with which arguments — are re-written verbatim inside ten handlers.

Each of the ten copies independently decides: that the not-found arm is built with `crate::state::not_found(crate::state::CLUSTER, &input.cluster_identifier)` (the CLUSTER kind, and the identifier exactly as received rather than a resolved id), and that the denied arm is built with `cluster_denied(&caller, Self::NAME, &input.cluster_identifier)` (the caller principal read before the lock, the operation's own name as the IAM action, and again the raw identifier). Any one copy can drift from the other nine — passing a resolved id, the wrong `ResourceKind`, a hard-coded operation name, or an omitted `caller` — and nothing in the crate would notice: the ten copies are pinned by three different test suites (eight rows in one, and the two update handlers in two other files), so no single suite covers them all.

This is the same shape of finding as the already-FIXED `pcs-createcluster-maintainability` (the tag-count ceiling hand-inlined in all three create handlers instead of going through the state helper `TagResource` uses): the rule has a home module, and the handlers bypass half of it.

The fix is confined to that module: let `cluster_authorization` build both exceptions (e.g. an enum carrying `ResourceNotFoundException` / `AccessDeniedException`, asked with the caller, the operation name and the identifier), so each handler is left with only the mapping into its own generated `*ErrorModeled` variant — which is the one part that genuinely cannot be shared, because each operation has its own error enum. Every response body, every error code and every existing test stay byte-for-byte identical.

## Evidence

THE RULE'S HOME, which says it is asked in one place — src/state/cluster_authorization.rs:95-101:
```
/// Which of the two refusals an unresolvable `cluster_identifier` earns.
///
/// The question is the identifier's FORM, which is what `ClusterIdentifier`'s own pattern
/// draws: ...
/// Asked by every cluster-scoped child operation, so none of them can word this differently.
```
and it returns only the discriminant — src/state/cluster_authorization.rs:109-114:
```
pub fn unresolved_cluster(cluster_identifier: &str) -> UnresolvedCluster {
    match cluster_identifier.starts_with(crate::state::GENERATED_ID_PREFIX) {
        true => UnresolvedCluster::NotFound,
        false => UnresolvedCluster::Denied,
    }
}
```

COPY 1 — src/ops/get_queue.rs:183-204:
```
        let Some(cluster) = state.cluster(&input.cluster_identifier) else {
            return Err(match crate::state::cluster_authorization::unresolved_cluster(
                &input.cluster_identifier,
            ) {
                crate::state::cluster_authorization::UnresolvedCluster::NotFound => {
                    GetQueueErrorModeled::ResourceNotFoundException(crate::state::not_found(
                        crate::state::CLUSTER,
                        &input.cluster_identifier,
                    ))
                }
                crate::state::cluster_authorization::UnresolvedCluster::Denied => {
                    GetQueueErrorModeled::AccessDeniedException(
                        crate::state::cluster_authorization::cluster_denied(
                            &caller,
                            Self::NAME,
                            &input.cluster_identifier,
                        ),
                    )
                }
            }
            .into());
        };
```

COPY 2 — the identical branch, differing only in the error enum name — src/ops/list_queues.rs:194-215:
```
        let Some(cluster) = state.cluster(&input.cluster_identifier) else {
            return Err(match crate::state::cluster_authorization::unresolved_cluster(
                &input.cluster_identifier,
            ) {
                crate::state::cluster_authorization::UnresolvedCluster::NotFound => {
                    ListQueuesErrorModeled::ResourceNotFoundException(crate::state::not_found(
                        crate::state::CLUSTER,
                        &input.cluster_identifier,
                    ))
                }
                crate::state::cluster_authorization::UnresolvedCluster::Denied => {
                    ListQueuesErrorModeled::AccessDeniedException(
                        crate::state::cluster_authorization::cluster_denied(
                            &caller,
                            Self::NAME,
                            &input.cluster_identifier,
                        ),
                    )
                }
            }
            .into());
        };
```

THE OTHER EIGHT COPIES of the same branch (grep `unresolved_cluster`):
- src/ops/create_compute_node_group.rs:357-380 (the first handler on the checklist that calls it)
- src/ops/create_queue.rs:268-289
- src/ops/update_queue.rs:230-251
- src/ops/update_compute_node_group.rs:327-347
- src/ops/delete_queue.rs:196-217
- src/ops/delete_compute_node_group.rs:197-220
- src/ops/get_compute_node_group.rs:185-208
- src/ops/list_compute_node_groups.rs:196-219

TEST COVERAGE that must stay unchanged (and that no single suite covers all ten with):
- tests/all/cluster_scoped_refusals.rs:249-254 `a_cluster_scoped_call_naming_no_cluster_is_refused_about_the_caller`, over the rows built at tests/all/cluster_scoped_refusals.rs:114-159 — which list only GetQueue, ListQueues, CreateQueue, DeleteQueue, GetComputeNodeGroup, ListComputeNodeGroups, CreateComputeNodeGroup, DeleteComputeNodeGroup (UpdateQueue and UpdateComputeNodeGroup are absent)
- tests/all/cluster_scoped_refusals.rs:444 `a_cluster_scoped_delete_is_refused_by_what_its_cluster_identifier_resolves_to`
- tests/all/cluster_scoped_refusals/absent_generated_cluster_id.rs:73
- tests/all/cluster_update_and_endpoints/queue_update.rs:383-423 (the UpdateQueue copy: `pcs_...` -> ResourceNotFoundException, a name -> AccessDeniedException)
- tests/all/cluster_update_and_endpoints/compute_node_group_update.rs:427 (the UpdateComputeNodeGroup copy)
- src/state/cluster_authorization.rs:180-203 pins the denial WORDING unit-wise

## Steps to Reproduce

1. grep -n "unresolved_cluster" services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/*.rs — ten handlers each contain the same two-arm match.
2. Open src/ops/get_queue.rs:183-204 and src/ops/list_queues.rs:194-215 side by side: the bodies are character-identical apart from the `*ErrorModeled` enum name.
3. Open src/state/cluster_authorization.rs:95-114: the module that claims the rule so "none of them can word this differently" returns only the two-valued discriminant, so each handler restates which exception each arm builds and with which arguments.
4. Confirm the drift surface: edit one copy (e.g. src/ops/list_compute_node_groups.rs:202-205) to pass the resolved cluster id instead of `&input.cluster_identifier`, or to drop `&caller`; `cargo test -p service-com-amazonaws-pcs-awsparallelcomputingservice` still fails only for the row that happens to be listed in tests/all/cluster_scoped_refusals.rs:114-159 — no suite checks the ten copies against one source of truth.
5. Compare with the already-fixed precedent (issue 20261118T121319Z-pcs-createcluster-maintainability): the tag-count ceiling was hand-inlined in the three create handlers and was moved behind the state helper, leaving responses and tests unchanged — the same move applies here.
