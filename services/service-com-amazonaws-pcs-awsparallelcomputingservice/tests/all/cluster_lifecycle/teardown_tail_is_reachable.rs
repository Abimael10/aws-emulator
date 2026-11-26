// SPDX-License-Identifier: CC0-1.0

//! The TAIL of a PCS build-then-teardown workflow: the legs a client reaches only once
//! every lifecycle wait before them has settled. Driven over the awsJson1_0 wire through
//! the same router the emulator serves from.
//!
//! RULE under test: a lifecycle transition this service owes a resource is materialised
//! before the NEXT request is answered, so a client that probes a `Get*` first and looks
//! again only on a miss spends exactly ONE probe per wait, however many waits its workflow
//! chains. A workflow therefore reaches its tail, where every resource whose removal the
//! service already owed is gone: each operation naming one is refused the modelled
//! `ResourceNotFoundException`, stating this service's fixed sentence for that KIND of
//! resource, carrying the identifier as the request supplied it in `resourceId` and the
//! kind of identifier in `resourceType`, the listings no longer hold it, and the cluster
//! that held them can then be deleted and disappears in turn.
//!
//! The sibling suite (`settles_on_the_next_request.rs`) states WHEN a single transition
//! becomes observable, and owns the one-request reader these tests poll through
//! ([`super::settles_on_the_next_request::observe`]). What these tests add is what a client
//! polling at the intervals the real service's provisioning time calls for depends on: that
//! no probe is wasted, so a chain of waits converges and the legs AFTER the last of them
//! are reachable at all.
//!
//! Nothing here sleeps and nothing asserts on elapsed time. A wait is counted in PROBES —
//! the unit a transition withheld from the next request costs a caller a whole polling
//! interval of.
//!
//! The rows span what the rule generalises over: how many compute node groups and queues
//! the cluster holds, so the number of chained waits differs per row; the form each call
//! addresses a resource by (its generated id or its name, the two forms the `*Identifier`
//! patterns admit); and, for the tail, every operation that names a child by its
//! identifier.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::{
    STATUS_ACTIVE, STATUS_CREATING, STATUS_DELETED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::settles_on_the_next_request::observe;
use super::{call, create_cluster, create_compute_node_group, create_queue, emulator};

/// The most probes one wait is given before the test gives up. Generous on purpose: a
/// transition settling several requests late would still be observed, so a failure reports
/// the probe COUNT a client would have paid rather than a timeout.
const PROBE_BUDGET: usize = 8;

/// How this service refuses a request about a kind of resource it cannot find: the fixed
/// sentence its `ResourceNotFoundException` states for that kind, and how it names the
/// kind of IDENTIFIER the request supplied, which is the `resourceType` such a refusal
/// carries.
#[derive(Debug, Clone, Copy)]
struct NotFound {
    /// The whole `message` member. Fixed text: it names the kind of resource and echoes
    /// no caller-supplied value, which travels in `resourceId` instead.
    sentence: &'static str,
    /// The `resourceType` member of an identifier-keyed not-found.
    identifier_kind: &'static str,
}

/// A queue this service cannot find.
const QUEUE_NOT_FOUND: NotFound = NotFound {
    sentence: "AWS PCS couldn't find the queue you specified.",
    identifier_kind: "Queue ID",
};

/// A compute node group this service cannot find.
const GROUP_NOT_FOUND: NotFound = NotFound {
    sentence: "AWS PCS couldn't find the compute node group you specified.",
    identifier_kind: "Compute Node Group ID",
};

/// How a request addresses a resource: by the generated `pcs_...` id, or by the
/// caller-chosen name. The two forms every identifier of this service admits
/// (`smithy_query(query="shape",shape="QueueIdentifier")`:
/// `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,25})$`).
#[derive(Debug, Clone, Copy)]
enum Address {
    /// The generated id the create minted.
    Id,
    /// The name the create asked for.
    Name,
}

/// The identifier a request addresses the resource by, in the form `address` asks for.
fn addressed(address: Address, id: &str, name: &str) -> String {
    match address {
        Address::Id => id.to_string(),
        Address::Name => name.to_string(),
    }
}

/// Poll `operation` the way a client written against the real service does — probe FIRST,
/// look again only on a miss — and answer how many probes the wait cost.
///
/// One probe is what a transition materialised for the next request costs. A transition
/// withheld until a span of wall-clock time had passed is what makes the same client pay
/// more, which is what the callers below assert against.
async fn probes_until(
    router: &MultiServiceRouter,
    case: &str,
    wait: &str,
    operation: &str,
    member: &str,
    request: &Value,
    settled: impl Fn(Option<&str>) -> bool,
) -> usize {
    for probes in 1..=PROBE_BUDGET {
        let observed = observe(router, case, operation, member, request).await;
        if settled(observed.as_deref()) {
            return probes;
        }
    }
    panic!(
        "case {case}: {wait} was still unsettled after {PROBE_BUDGET} probes of {operation} \
         {request}"
    );
}

/// Whether a probe reports the resource provisioned.
fn active(observed: Option<&str>) -> bool {
    observed == Some(STATUS_ACTIVE)
}

/// Whether a probe reports the resource gone — what the reader answers `None` for.
fn gone(observed: Option<&str>) -> bool {
    observed.is_none()
}

/// Whether a probe reports a compute node group taken out: `ComputeNodeGroupStatus` is the
/// one of this service's three status enums that declares [`STATUS_DELETED`], so a client
/// tearing a group down waits for "gone or `DELETED`".
fn gone_or_deleted(observed: Option<&str>) -> bool {
    gone(observed) || observed == Some(STATUS_DELETED)
}

/// Refuse-and-check: `operation` is refused the modelled not-found for `kind`, naming
/// `identifier` exactly as the request supplied it.
async fn assert_not_found(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    kind: NotFound,
    identifier: &str,
) {
    let (status, refused) = call(router, operation, request).await;
    assert_eq!(
        (status, refused["__type"].as_str()),
        (CLIENT_ERROR_STATUS, Some("ResourceNotFoundException")),
        "case {case}: expected {operation} {request} to be refused as a not-found, got \
         {status} with {refused}"
    );
    assert_eq!(
        refused["message"].as_str(),
        Some(kind.sentence),
        "case {case}: expected {operation} to state the service's own sentence for the kind \
         of resource it could not find, got {refused}"
    );
    assert_eq!(
        (
            refused["resourceId"].as_str(),
            refused["resourceType"].as_str()
        ),
        (Some(identifier), Some(kind.identifier_kind)),
        "case {case}: expected {operation} to name the identifier it could not resolve, got \
         {refused}"
    );
}

/// How many resources a listing of `member` inside `cluster` holds.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    member: &str,
    cluster: &Value,
) -> usize {
    let (status, body) = call(router, operation, cluster).await;
    assert_eq!(
        status, 200,
        "case {case}: expected {operation} {cluster} to be served, got {status} with {body}"
    );
    match body[member].as_array() {
        Some(entries) => entries.len(),
        None => panic!("case {case}: expected {operation} to answer a `{member}` list, got {body}"),
    }
}

/// RULE: a workflow that builds a cluster's hierarchy and tears it down again pays ONE
/// probe per lifecycle wait — the probe issued straight after the call that starts the
/// transition — and the legs after its last wait are reached: re-deleting a child the
/// service has already taken out is refused the modelled not-found, and the cluster's own
/// delete is then served.
#[tokio::test]
async fn a_build_then_teardown_workflow_costs_one_probe_per_wait_and_reaches_its_tail() {
    // Rows: how many compute node groups and queues the cluster holds — so the row's
    // workflow chains 2 + 2 * (groups + queues) waits — and the form every call in it
    // addresses a resource by.
    let rows: &[(&str, usize, usize, Address)] = &[
        ("one group, one queue, addressed by id", 1, 1, Address::Id),
        (
            "one group, one queue, addressed by name",
            1,
            1,
            Address::Name,
        ),
        ("two groups, two queues, addressed by id", 2, 2, Address::Id),
        (
            "three groups, one queue, addressed by name",
            3,
            1,
            Address::Name,
        ),
    ];

    for (case, group_count, queue_count, address) in rows {
        let router = emulator();
        // What the row's workflow waits for, and what polling for it actually cost.
        let mut waits = 0usize;
        let mut probes = 0usize;

        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster("teardown-cluster", "SMALL"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateCluster got {status} with {created}"
        );
        assert_eq!(
            created["cluster"]["status"],
            json!(STATUS_CREATING),
            "case {case}: expected CreateCluster to report the transient status, got {created}"
        );
        let cluster_ref = addressed(
            *address,
            created["cluster"]["id"].as_str().unwrap_or_default(),
            "teardown-cluster",
        );
        let read_cluster = json!({"clusterIdentifier": &cluster_ref});
        waits += 1;
        probes += probes_until(
            &router,
            case,
            "the cluster becoming ACTIVE",
            "GetCluster",
            "cluster",
            &read_cluster,
            active,
        )
        .await;

        // The compute node groups, each one wait of its own.
        let mut groups: Vec<(String, Value)> = Vec::new();
        for index in 0..*group_count {
            let name = format!("teardown-nodes-{index}");
            let (status, created) = call(
                &router,
                "CreateComputeNodeGroup",
                &create_compute_node_group(&cluster_ref, &name),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: CreateComputeNodeGroup {name} got {status} with {created}"
            );
            assert_eq!(
                created["computeNodeGroup"]["status"],
                json!(STATUS_CREATING),
                "case {case}: expected CreateComputeNodeGroup {name} to report the transient \
                 status, got {created}"
            );
            let group_ref = addressed(
                *address,
                created["computeNodeGroup"]["id"].as_str().unwrap_or_default(),
                &name,
            );
            let read_group = json!({
                "clusterIdentifier": &cluster_ref,
                "computeNodeGroupIdentifier": &group_ref,
            });
            waits += 1;
            probes += probes_until(
                &router,
                case,
                "the compute node group becoming ACTIVE",
                "GetComputeNodeGroup",
                "computeNodeGroup",
                &read_group,
                active,
            )
            .await;
            groups.push((group_ref, read_group));
        }

        // The queues, each assigning jobs to the row's first group.
        let first_group = groups
            .first()
            .map(|(group_ref, _)| group_ref.clone())
            .unwrap_or_default();
        let mut queues: Vec<(String, Value)> = Vec::new();
        for index in 0..*queue_count {
            let name = format!("teardown-work-{index}");
            let (status, created) = call(
                &router,
                "CreateQueue",
                &create_queue(&cluster_ref, &name, &[first_group.as_str()]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: CreateQueue {name} got {status} with {created}"
            );
            assert_eq!(
                created["queue"]["status"],
                json!(STATUS_CREATING),
                "case {case}: expected CreateQueue {name} to report the transient status, got \
                 {created}"
            );
            let queue_ref = addressed(
                *address,
                created["queue"]["id"].as_str().unwrap_or_default(),
                &name,
            );
            let read_queue = json!({
                "clusterIdentifier": &cluster_ref,
                "queueIdentifier": &queue_ref,
            });
            waits += 1;
            probes += probes_until(
                &router,
                case,
                "the queue becoming ACTIVE",
                "GetQueue",
                "queue",
                &read_queue,
                active,
            )
            .await;
            queues.push((queue_ref, read_queue));
        }

        // Teardown, in dependency order: the queues first, then the groups they assigned
        // jobs to.
        for (queue_ref, read_queue) in &queues {
            let (status, deleted) = call(&router, "DeleteQueue", read_queue).await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteQueue {read_queue} got {status} with {deleted}"
            );
            waits += 1;
            probes += probes_until(
                &router,
                case,
                "the queue disappearing",
                "GetQueue",
                "queue",
                read_queue,
                gone,
            )
            .await;
            // The tail of the recorded workflow: the queue's removal has come due, so the
            // service no longer holds it and deleting it AGAIN names a resource that is not
            // there.
            assert_not_found(
                &router,
                case,
                "DeleteQueue",
                read_queue,
                QUEUE_NOT_FOUND,
                queue_ref,
            )
            .await;
        }
        for (group_ref, read_group) in &groups {
            let (status, deleted) = call(&router, "DeleteComputeNodeGroup", read_group).await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteComputeNodeGroup {read_group} got {status} with {deleted}"
            );
            waits += 1;
            probes += probes_until(
                &router,
                case,
                "the compute node group disappearing",
                "GetComputeNodeGroup",
                "computeNodeGroup",
                read_group,
                gone_or_deleted,
            )
            .await;
            assert_not_found(
                &router,
                case,
                "DeleteComputeNodeGroup",
                read_group,
                GROUP_NOT_FOUND,
                group_ref,
            )
            .await;
        }

        // Nothing of the cluster's is left to block its own delete, which is the leg the
        // whole chain of waits leads to.
        let (status, deleted) = call(&router, "DeleteCluster", &read_cluster).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteCluster got {status} with {deleted}"
        );
        waits += 1;
        probes += probes_until(
            &router,
            case,
            "the cluster disappearing",
            "GetCluster",
            "cluster",
            &read_cluster,
            gone,
        )
        .await;

        assert_eq!(
            probes, waits,
            "case {case}: expected each of the workflow's {waits} waits to settle on its \
             first probe, got {probes} probes"
        );
    }
}

/// RULE, on the other side of a settled removal: once a child's removal has come due,
/// EVERY operation that names it by its identifier is refused the same modelled not-found,
/// and the cluster's listings no longer hold it.
#[tokio::test]
async fn every_operation_naming_a_removed_child_is_refused_the_same_not_found() {
    // Rows: the form each call addresses its resources by.
    let rows: &[(&str, Address)] = &[
        ("addressed by id", Address::Id),
        ("addressed by name", Address::Name),
    ];

    for (case, address) in rows {
        let router = emulator();
        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster("removed-cluster", "SMALL"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateCluster got {status} with {created}"
        );
        let cluster_ref = addressed(
            *address,
            created["cluster"]["id"].as_str().unwrap_or_default(),
            "removed-cluster",
        );
        let read_cluster = json!({"clusterIdentifier": &cluster_ref});

        let (status, created) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_compute_node_group(&cluster_ref, "removed-nodes"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateComputeNodeGroup got {status} with {created}"
        );
        let group_ref = addressed(
            *address,
            created["computeNodeGroup"]["id"].as_str().unwrap_or_default(),
            "removed-nodes",
        );
        let read_group = json!({
            "clusterIdentifier": &cluster_ref,
            "computeNodeGroupIdentifier": &group_ref,
        });

        let (status, created) = call(
            &router,
            "CreateQueue",
            &create_queue(&cluster_ref, "removed-work", &[group_ref.as_str()]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateQueue got {status} with {created}"
        );
        let queue_ref = addressed(
            *address,
            created["queue"]["id"].as_str().unwrap_or_default(),
            "removed-work",
        );
        let read_queue = json!({
            "clusterIdentifier": &cluster_ref,
            "queueIdentifier": &queue_ref,
        });

        // The queue's delete is accepted, so its removal is owed from the next request on.
        let (status, deleted) = call(&router, "DeleteQueue", &read_queue).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteQueue got {status} with {deleted}"
        );

        // Rows: every operation that names a queue by `queueIdentifier` — the reader, the
        // update and the delete. Each is refused the same way, so what a client reads off
        // the refusal does not depend on which call it was making.
        for operation in ["GetQueue", "UpdateQueue", "DeleteQueue"] {
            assert_not_found(
                &router,
                case,
                operation,
                &read_queue,
                QUEUE_NOT_FOUND,
                &queue_ref,
            )
            .await;
        }
        assert_eq!(
            listed(&router, case, "ListQueues", "queues", &read_cluster).await,
            0,
            "case {case}: expected ListQueues to have dropped the queue it no longer holds"
        );

        // The compute node group: its own status enum declares DELETED, so the request after
        // its delete reports that, and the record is forgotten by the one after.
        let (status, deleted) = call(&router, "DeleteComputeNodeGroup", &read_group).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteComputeNodeGroup got {status} with {deleted}"
        );
        assert_eq!(
            observe(
                &router,
                case,
                "GetComputeNodeGroup",
                "computeNodeGroup",
                &read_group
            )
            .await
            .as_deref(),
            Some(STATUS_DELETED),
            "case {case}: expected the group DELETED on the request that follows its delete"
        );
        for operation in [
            "GetComputeNodeGroup",
            "UpdateComputeNodeGroup",
            "DeleteComputeNodeGroup",
        ] {
            assert_not_found(
                &router,
                case,
                operation,
                &read_group,
                GROUP_NOT_FOUND,
                &group_ref,
            )
            .await;
        }
        assert_eq!(
            listed(
                &router,
                case,
                "ListComputeNodeGroups",
                "computeNodeGroups",
                &read_cluster
            )
            .await,
            0,
            "case {case}: expected ListComputeNodeGroups to have dropped the group it no \
             longer holds"
        );

        // And with both children gone the cluster's own delete is served and settles too.
        let (status, deleted) = call(&router, "DeleteCluster", &read_cluster).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteCluster got {status} with {deleted}"
        );
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster).await,
            None,
            "case {case}: expected the cluster gone on the request that follows its delete"
        );
    }
}
