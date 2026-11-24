Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 228e46deaa0791471f64389b8b4e4da29945ad74

# The record-count ServiceQuotaExceededException states an emulator-authored sentence that publishes the emulator's own ceiling, instead of the sentence the crate's own constants document for that refusal

_Filed: 2026-11-21T00:40:13Z_
_Operation: CreateCluster | Class: error-response_

> _Note: this issue was identified against the code base at commit `228e46deaa0791471f64389b8b4e4da29945ad74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::quota_exceeded` is the shared builder of the modelled `ServiceQuotaExceededException` that `CreateCluster` (first caller), `CreateQueue` and `CreateComputeNodeGroup` answer when a record-count ceiling is reached. It authors its own sentence, `"the maximum number of {resource_type} resources ({limit}) has been reached"`, interpolating two values no sentence of this service carries:

1. the emulator's OWN ceiling (`MAX_CLUSTERS` = 50, `MAX_QUEUES_PER_CLUSTER` = 50, `MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER` = 20) — the same constants whose doc comments say in so many words that the number is "the emulator's own ceiling, not a published AWS default"; and
2. the ARN wire token the call sites pass as a bare string literal (`"cluster"`, `"queue"`, `"computenodegroup"`), so the prose reads "the maximum number of computenodegroup resources (20) has been reached" even though this crate keeps a `ResourceKind.noun` ("compute node group") precisely so an error SENTENCE never carries the wire token (`state.rs:1104-1138`).

Those same constants' doc comments quote the sentences this refusal is stated with — "The max number of clusters or queues has been reached for the account" and "The max number of compute node groups has been reached for the associated cluster" — which are also the examples `ServiceQuotaExceededException`'s own model documentation gives, reproduced verbatim in every create handler's header. So the crate writes down the sentence for this refusal in one place and then answers a different, self-authored one.

It is also the ONE refusal of this service that does so. Every other refusal is built from the service's own fixed text and echoes no supplied value (`not_found`, `cluster_not_active`, `compute_node_group_still_referenced`, `cluster_still_has_children`, each with a comment saying so), and the sibling quota builder in the SAME module, `tag_quota_exceeded`, states AWS's resulting-set sentence and justifies putting a ceiling in it only because "AWS states the resulting-set rule and the ceiling". The record-count ceiling is not a rule or a number AWS states, yet it is published in the message.

Effect on a client: the body of a quota refusal (HTTP 400 for this service's client-error class, `__type` `ServiceQuotaExceededException`) carries emulator-internal prose and an emulator-internal number, so a client that logs or matches the sentence sees something the service never says, and the text changes with an emulator constant.

## Evidence

The authored sentence — services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/state.rs:1488-1501:
```
/// The modelled `ServiceQuotaExceededException` (HTTP 402) for a create that would take
/// a store past its ceiling. `quota_code` is left absent rather than invented: the
/// emulator stands in for the quota, it does not reproduce Service Quotas' codes.
pub fn quota_exceeded(resource_type: &str, limit: usize) -> ServiceQuotaExceededException {
    ServiceQuotaExceededException {
        message: format!(
            "the maximum number of {resource_type} resources ({limit}) has been reached"
        ),
```

The sentence the crate itself writes down for exactly this refusal, together with the admission that the number is the emulator's — src/state.rs:174-179:
```
/// The most clusters this emulator holds. PCS caps clusters per account per Region with
/// an account-adjustable service quota and answers `ServiceQuotaExceededException` once
/// it is reached ("The max number of clusters or queues has been reached for the
/// account"); the NUMBER here is the emulator's own ceiling, not a published AWS
/// default, and exists so a repeated create cannot grow this process without bound.
pub const MAX_CLUSTERS: usize = 50;
```
and src/state.rs:181-184:
```
/// The most compute node groups one cluster holds, refused the same way ("The max number
/// of compute node groups has been reached for the associated cluster"). The number is
/// the emulator's ceiling.
pub const MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER: usize = 20;
```

The same two sentences are the error's own modelled examples, as rendered in the handler header — src/ops/create_cluster.rs:95-101: "* `ServiceQuotaExceededException` (HTTP 402) — You exceeded your service quota. ... Examples The max number of clusters or queues has been reached for the account. The max number of compute node groups has been reached for the associated cluster. The total of maxInstances across all compute node groups has been reached for associated cluster."

The sibling quota builder in the same module, which DOES state the service's sentence and explains when a ceiling may appear in one — src/state.rs:1543-1551:
```
/// AWS states the resulting-set rule and the ceiling, and names neither the request's
/// keys nor the resource, so the only variable part of the sentence is the ceiling
/// itself. ...
pub fn tag_quota_exceeded(kind: ResourceKind, resource_id: &str) -> ServiceQuotaExceededException {
    ServiceQuotaExceededException {
        message: format!(
            "The resultant tag set must not have more than {MAX_TAGS_PER_RESOURCE} user tags."
        ),
```

The wire token reaching the prose, with the crate's own distinction between a token and a noun — src/state.rs:1108-1138: `pub struct ResourceKind { /// The `resourceType` member of `ConflictException` ... pub resource_type: &'static str, /// How an error sentence names this kind of resource. pub noun: &'static str, ...}` with `COMPUTE_NODE_GROUP = ResourceKind { resource_type: "computenodegroup", noun: "compute node group", ... }` — while the call sites hand `quota_exceeded` the token as a literal: src/ops/create_cluster.rs:326-331 `crate::state::quota_exceeded("cluster", crate::state::MAX_CLUSTERS)`, src/ops/create_queue.rs:348-353 `crate::state::quota_exceeded("queue", crate::state::MAX_QUEUES_PER_CLUSTER)`, src/ops/create_compute_node_group.rs:440-450 `crate::state::quota_exceeded("computenodegroup", crate::state::MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER)` (note create_queue.rs:342 uses `crate::state::QUEUE.resource_type` for the conflict's wire member eight lines earlier, so both forms sit in one handler).

Test coverage: the two suites that drive these ceilings assert only the status and the error code, never the message — tests/all/cluster_lifecycle.rs:991-996 `assert_eq!((status, refused["__type"].as_str()), (CLIENT_ERROR_STATUS, Some("ServiceQuotaExceededException")), ...)` and tests/all/cluster_lifecycle.rs:1026-1053 for the per-cluster group and queue ceilings — so the sentence is unpinned and correcting it breaks no existing test. The tag-quota message is pinned separately in tests/all/resource_tags.rs:338+ and is unaffected.

## Steps to Reproduce

1. Create 50 clusters (the emulator's `MAX_CLUSTERS`), each settling to ACTIVE on the following request.
2. Call CreateCluster once more with a fresh `clusterName`.
3. Observe the refusal body: `__type` `ServiceQuotaExceededException` with `message` "the maximum number of cluster resources (50) has been reached" — an emulator-authored sentence carrying the emulator's own ceiling, where this crate's own `MAX_CLUSTERS` doc (src/state.rs:174-179) and the error's modelled examples (src/ops/create_cluster.rs:98-100) state the refusal as "The max number of clusters or queues has been reached for the account".
4. Create a cluster, then 20 compute node groups in it (`MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER`), then one more.
5. Observe `message` "the maximum number of computenodegroup resources (20) has been reached" — the ARN wire token in the prose, where the crate holds a `noun` ("compute node group") for sentences and documents this refusal as "The max number of compute node groups has been reached for the associated cluster".
6. Fill a cluster's queues to `MAX_QUEUES_PER_CLUSTER` and create one more: same shape, "the maximum number of queue resources (50) has been reached".
