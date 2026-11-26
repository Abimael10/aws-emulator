Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: ee24c053d2f664e1aece2bc382274e6b27f60b27

# The resource tag-count ceiling is hand-inlined in all three create handlers instead of going through the state helper TagResource uses

_Filed: 2026-11-18T12:13:19Z_
_Operation: CreateCluster | Class: maintainability_

> _Note: this issue was identified against the code base at commit `ee24c053d2f664e1aece2bc382274e6b27f60b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "a resource holds at most MAX_TAGS_PER_RESOURCE tags, and a write that would exceed it is refused with tag_quota_exceeded" is written four times in this crate: once through the state helper (`tag_count_after_merge`, used by TagResource) and three more times inline in CreateCluster, CreateComputeNodeGroup and CreateQueue, each re-deriving the count with `input.tags.as_ref().map_or(0, HashMap::len)` and repeating the same four-line comment. The state module claims the rule is checked "on every path that writes tags" — but only one of the four paths goes through the shared counting helper, so the three creates can drift from TagResource (e.g. if the ceiling ever became per-kind, or the count ever had to consider keys a create also supplies elsewhere) without any compile error.

The creates start from an empty stored set, so routing them through the existing helper (or a thin `state` function wrapping the comparison and the refusal) changes no response and no test: behaviour is identical today, the duplication is the defect.

## Evidence

The state module states the rule as one rule — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/state.rs:39-42:
```
//! * tag COUNT — a resource's stored tag set is capped at [`MAX_TAGS_PER_RESOURCE`]
//!   ([`tag_quota_exceeded`]) on every path that writes tags, a create carrying `tags`
//!   as much as a later `TagResource`; `RequestTagMap`'s modelled
//!   `@length(min: 1, max: 200)` bounds one request only.
```
The shared counting helper, called by exactly one path — src/state.rs:1381-1390 (`tag_count_after_merge`) via src/ops/tag_resource.rs:177-183:
```
        let resulting = crate::state::tag_count_after_merge(stored, &input.tags);
        if resulting > crate::state::MAX_TAGS_PER_RESOURCE {
            return Err(TagResourceErrorModeled::ServiceQuotaExceededException(
                crate::state::tag_quota_exceeded(kind, &resource_id),
```
Copy 2 — src/ops/create_cluster.rs:265-271:
```
        let requested_tags = input.tags.as_ref().map_or(0, std::collections::HashMap::len);
        if requested_tags > crate::state::MAX_TAGS_PER_RESOURCE {
            return Err(CreateClusterErrorModeled::ServiceQuotaExceededException(
                crate::state::tag_quota_exceeded(crate::state::CLUSTER, &input.cluster_name),
```
Copy 3 — src/ops/create_compute_node_group.rs:284-295 (same two lines, `COMPUTE_NODE_GROUP`, `&input.compute_node_group_name`).
Copy 4 — src/ops/create_queue.rs:238-244 (same two lines, `QUEUE`, `&input.queue_name`).

Tests that pin both sides and must stay green unchanged: tests/all/resource_tags.rs:338 `a_tag_write_past_the_resource_tag_quota_is_refused_and_persists_nothing` — the TagResource ceiling at lines 492-520 and the create-path ceiling at lines 522-571 ("a create at the ceiling" -> 200, "a create one past the ceiling" -> ServiceQuotaExceededException and no resource created).

## Steps to Reproduce

1. Open src/ops/create_cluster.rs:265, src/ops/create_compute_node_group.rs:284 and src/ops/create_queue.rs:238: three copies of the same count-vs-MAX_TAGS_PER_RESOURCE comparison plus the same refusal construction.
2. Open src/ops/tag_resource.rs:177: the fourth copy, the only one using the state helper `tag_count_after_merge` (src/state.rs:1381).
3. Change the rule in one place only (for example make the ceiling depend on the resource kind) and observe that nothing forces the other three copies to follow.
4. Route the three creates through the existing helper (an empty stored set) so the comparison and the refusal live once; re-run tests/all/resource_tags.rs — every response and every assertion is unchanged.
