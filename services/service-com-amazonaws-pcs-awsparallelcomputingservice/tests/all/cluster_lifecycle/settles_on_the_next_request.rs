// SPDX-License-Identifier: CC0-1.0

//! WHEN a PCS lifecycle transition becomes observable, end to end over the awsJson1_0 wire
//! through the same router the emulator serves from.
//!
//! RULE under test: a transition this service owes a resource is due from the instant the
//! call that starts it answers, and is materialised before the NEXT request is answered.
//! The call that starts it still reports the transient status the real service reports
//! (`CREATING`, `UPDATING`, `DELETING`), and the first probe a caller issues after it
//! already reports the settled resource — `ACTIVE`, `DELETED` where the status enum
//! declares it, or gone. Nothing is deferred behind wall-clock time, so a client whose
//! polling interval is sized against the minutes the real service takes never pays an
//! interval for a transition this emulator has already decided, and a workflow that chains
//! several lifecycle waits converges in one probe per wait.
//!
//! What that does NOT change: a resource the service has not finished provisioning (or is
//! taking out) still refuses the operations that need it settled, with the modelled
//! `ConflictException`. The last two suites hold a record in such a status through this
//! service's own store — the only way to hold one there, now that no request leaves a
//! resource transient — and check both that refusal and the transient status a delete
//! records.
//!
//! Every suite builds its own engine, so the state one writes is invisible to the others,
//! and nothing here sleeps: there is nothing to wait for.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::{
    service_state, PendingChange, STATUS_ACTIVE, STATUS_CREATING, STATUS_DELETED, STATUS_DELETING,
    STATUS_UPDATING,
};
use smithy_server_runtime::MultiServiceRouter;

use super::{
    call, create_cluster, create_compute_node_group, create_queue, emulator, emulator_with_engine,
};

/// One probe: the status `operation` reports for the resource `request` names, or `None`
/// when that reader answers the modelled `ResourceNotFoundException` because the resource
/// is gone.
///
/// Exactly one request, so a suite can count what a workflow costs a polling client.
async fn observe(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    member: &str,
    request: &Value,
) -> Option<String> {
    let (status, body) = call(router, operation, request).await;
    if status == CLIENT_ERROR_STATUS {
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case}: expected {operation} {request} to report a resource that is gone \
             as a not-found, got {body}"
        );
        return None;
    }
    assert_eq!(
        status, 200,
        "case {case}: expected {operation} {request} to be served, got {status} with {body}"
    );
    let reported = body[member]["status"].as_str().map(str::to_string);
    assert!(
        reported.is_some(),
        "case {case}: expected {operation} to report the resource's status, got {body}"
    );
    reported
}

/// An `UpdateCluster` request that changes one stored member, so the update is a real one
/// and the cluster is owed its settled status again.
fn update_cluster(identifier: &str, seconds: i64) -> Value {
    json!({
        "clusterIdentifier": identifier,
        "slurmConfiguration": {"scaleDownIdleTimeInSeconds": seconds},
    })
}

/// RULE: every transition this service owes — a create's, an update's and a delete's, on
/// each of the three resources PCS holds — is settled by the request that follows the call
/// that started it, while that call itself still reports the transient status.
#[tokio::test]
async fn every_owed_transition_is_settled_by_the_request_that_follows_it() {
    // Rows: how every call addresses the resources it names — the generated `pcs_...` id or
    // the caller-chosen name, the two forms `ClusterIdentifier`,
    // `ComputeNodeGroupIdentifier` and `QueueIdentifier` each allow.
    for by_name in [false, true] {
        let case = if by_name {
            "addressed by name"
        } else {
            "addressed by id"
        };
        let router = emulator();

        // A cluster create reports CREATING; the next request reports it ACTIVE.
        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster("settle-cluster", "SMALL"),
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
        let cluster_id = created["cluster"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let cluster_ref = if by_name {
            "settle-cluster".to_string()
        } else {
            cluster_id.clone()
        };
        let read_cluster = json!({"clusterIdentifier": &cluster_ref});
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster)
                .await
                .as_deref(),
            Some(STATUS_ACTIVE),
            "case {case}: expected the cluster to be ACTIVE on the request that follows its \
             create"
        );

        // A compute node group create: the same rule on the next resource down.
        let (status, created) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_compute_node_group(&cluster_ref, "settle-nodes"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateComputeNodeGroup got {status} with {created}"
        );
        assert_eq!(
            created["computeNodeGroup"]["status"],
            json!(STATUS_CREATING),
            "case {case}: expected CreateComputeNodeGroup to report the transient status, \
             got {created}"
        );
        let group_ref = if by_name {
            "settle-nodes".to_string()
        } else {
            created["computeNodeGroup"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let read_group = json!({
            "clusterIdentifier": &cluster_ref,
            "computeNodeGroupIdentifier": &group_ref,
        });
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
            Some(STATUS_ACTIVE),
            "case {case}: expected the compute node group to be ACTIVE on the request that \
             follows its create"
        );

        // A queue create, assigning jobs to that group.
        let (status, created) = call(
            &router,
            "CreateQueue",
            &create_queue(&cluster_ref, "settle-queue", &[group_ref.as_str()]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateQueue got {status} with {created}"
        );
        assert_eq!(
            created["queue"]["status"],
            json!(STATUS_CREATING),
            "case {case}: expected CreateQueue to report the transient status, got {created}"
        );
        let queue_ref = if by_name {
            "settle-queue".to_string()
        } else {
            created["queue"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let read_queue = json!({
            "clusterIdentifier": &cluster_ref,
            "queueIdentifier": &queue_ref,
        });
        assert_eq!(
            observe(&router, case, "GetQueue", "queue", &read_queue)
                .await
                .as_deref(),
            Some(STATUS_ACTIVE),
            "case {case}: expected the queue to be ACTIVE on the request that follows its \
             create"
        );

        // An update is owed the settled status the same way: UPDATING in the update's own
        // answer, ACTIVE on the next request.
        let (status, updated) =
            call(&router, "UpdateCluster", &update_cluster(&cluster_ref, 600)).await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateCluster got {status} with {updated}"
        );
        assert_eq!(
            updated["cluster"]["status"],
            json!(STATUS_UPDATING),
            "case {case}: expected UpdateCluster to report the transient status, got {updated}"
        );
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster)
                .await
                .as_deref(),
            Some(STATUS_ACTIVE),
            "case {case}: expected the cluster to be ACTIVE again on the request that \
             follows its update"
        );

        // A delete answers an empty body, and the removal it owes is materialised before
        // the next request: the queue is already gone for the probe that follows it.
        let (status, deleted) = call(&router, "DeleteQueue", &read_queue).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteQueue got {status} with {deleted}"
        );
        assert_eq!(
            observe(&router, case, "GetQueue", "queue", &read_queue).await,
            None,
            "case {case}: expected the queue to be gone on the request that follows its \
             delete"
        );

        // The compute node group is the one resource whose status enum declares DELETED, so
        // its delete owes that status on the next request and the removal on the one after.
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
            "case {case}: expected the compute node group to be DELETED on the request that \
             follows its delete"
        );
        assert_eq!(
            observe(
                &router,
                case,
                "GetComputeNodeGroup",
                "computeNodeGroup",
                &read_group
            )
            .await,
            None,
            "case {case}: expected the compute node group to be forgotten on the request \
             after the one that reported it DELETED"
        );

        // And the cluster, whose status enum declares no DELETED: DELETING, then gone.
        let (status, deleted) = call(&router, "DeleteCluster", &read_cluster).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteCluster got {status} with {deleted}"
        );
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster).await,
            None,
            "case {case}: expected the cluster to be gone on the request that follows its \
             delete"
        );
    }
}

/// RULE, chained: every wait in a workflow that builds and tears a hierarchy down is
/// settled on its FIRST probe — the probe issued immediately after the call, which is where
/// a client written against the real service would otherwise sleep a whole polling interval
/// before looking again. Each [`observe`] below is that one probe.
#[tokio::test]
async fn a_chained_workflow_sees_each_wait_settled_on_its_first_probe() {
    // Rows: how many compute node groups and queues the cluster holds, so the number of
    // lifecycle waits the workflow chains differs per row.
    let rows: &[(&str, usize, usize)] = &[
        ("one group, one queue", 1, 1),
        ("two groups, two queues", 2, 2),
        ("one group, three queues", 1, 3),
    ];

    for (case, group_count, queue_count) in rows {
        let router = emulator();

        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster("chained-cluster", "SMALL"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateCluster got {status} with {created}"
        );
        let cluster_id = created["cluster"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read_cluster = json!({"clusterIdentifier": &cluster_id});
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster)
                .await
                .as_deref(),
            Some(STATUS_ACTIVE),
            "case {case}: expected the cluster ACTIVE on the first probe after its create"
        );

        let mut groups: Vec<Value> = Vec::new();
        for index in 0..*group_count {
            let name = format!("chained-nodes-{index}");
            let (status, created) = call(
                &router,
                "CreateComputeNodeGroup",
                &create_compute_node_group(&cluster_id, &name),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: CreateComputeNodeGroup {name} got {status} with {created}"
            );
            let read = json!({
                "clusterIdentifier": &cluster_id,
                "computeNodeGroupIdentifier": created["computeNodeGroup"]["id"]
                    .as_str()
                    .unwrap_or_default(),
            });
            assert_eq!(
                observe(&router, case, "GetComputeNodeGroup", "computeNodeGroup", &read)
                    .await
                    .as_deref(),
                Some(STATUS_ACTIVE),
                "case {case}: expected {name} ACTIVE on the first probe after its create"
            );
            groups.push(read);
        }

        let mut queues: Vec<Value> = Vec::new();
        for index in 0..*queue_count {
            let name = format!("chained-work-{index}");
            let (status, created) =
                call(&router, "CreateQueue", &create_queue(&cluster_id, &name, &[])).await;
            assert_eq!(
                status, 200,
                "case {case}: CreateQueue {name} got {status} with {created}"
            );
            let read = json!({
                "clusterIdentifier": &cluster_id,
                "queueIdentifier": created["queue"]["id"].as_str().unwrap_or_default(),
            });
            assert_eq!(
                observe(&router, case, "GetQueue", "queue", &read)
                    .await
                    .as_deref(),
                Some(STATUS_ACTIVE),
                "case {case}: expected {name} ACTIVE on the first probe after its create"
            );
            queues.push(read);
        }

        // Teardown, in dependency order. Each delete's first probe already reports the
        // resource settled: gone, or DELETED for the compute node group, whose status enum
        // is the one that declares that member.
        for read in &queues {
            let (status, deleted) = call(&router, "DeleteQueue", read).await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteQueue {read} got {status} with {deleted}"
            );
            assert_eq!(
                observe(&router, case, "GetQueue", "queue", read).await,
                None,
                "case {case}: expected the queue {read} gone on the first probe after its \
                 delete"
            );
        }
        for read in &groups {
            let (status, deleted) = call(&router, "DeleteComputeNodeGroup", read).await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteComputeNodeGroup {read} got {status} with {deleted}"
            );
            assert_eq!(
                observe(&router, case, "GetComputeNodeGroup", "computeNodeGroup", read)
                    .await
                    .as_deref(),
                Some(STATUS_DELETED),
                "case {case}: expected the compute node group {read} DELETED on the first \
                 probe after its delete"
            );
        }
        let (status, deleted) = call(&router, "DeleteCluster", &read_cluster).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteCluster got {status} with {deleted}"
        );
        assert_eq!(
            observe(&router, case, "GetCluster", "cluster", &read_cluster).await,
            None,
            "case {case}: expected the cluster gone on the first probe after its delete"
        );
    }
}

/// Hold the cluster `identifier` in `status` with nothing owed — a resource the real
/// service has not finished provisioning, or is still taking out, for as long as that work
/// takes. No sequence of requests leaves a PCS resource there, so this is written through
/// the service's own store.
fn hold_cluster(engine: &engine::InMemoryEngine, identifier: &str, status: &str) {
    let store = service_state(engine).expect("this service's own store");
    let mut state = store.lock();
    let cluster = state
        .cluster_mut(identifier)
        .expect("the cluster the create persisted");
    cluster.status = status.to_string();
    cluster.pending = None;
}

/// RULE: settling a transition by the next request does not relax the status gates — a
/// cluster that is not settled still refuses every operation that needs it `ACTIVE` with
/// the modelled `ConflictException`, naming the cluster in `resourceId`.
#[tokio::test]
async fn a_cluster_that_is_not_settled_refuses_what_needs_it_active() {
    // Rows: the unsettled `ClusterStatus` members a cluster can be held in — provisioning,
    // applying an update, and on its way out.
    for held in [STATUS_CREATING, STATUS_UPDATING, STATUS_DELETING] {
        let (router, engine) = emulator_with_engine();
        let (status, created) = call(
            &router,
            "CreateCluster",
            &create_cluster("gated-cluster", "SMALL"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {held}: CreateCluster got {status} with {created}"
        );
        let cluster_id = created["cluster"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        hold_cluster(&engine, "gated-cluster", held);

        // Each row: an operation that needs the cluster settled, and its request.
        let gated: [(&str, Value); 4] = [
            (
                "CreateComputeNodeGroup",
                create_compute_node_group("gated-cluster", "gated-nodes"),
            ),
            (
                "CreateQueue",
                create_queue("gated-cluster", "gated-queue", &[]),
            ),
            ("UpdateCluster", update_cluster("gated-cluster", 700)),
            (
                "DeleteCluster",
                json!({"clusterIdentifier": "gated-cluster"}),
            ),
        ];
        for (operation, request) in &gated {
            let (status, refused) = call(&router, operation, request).await;
            assert_eq!(
                status, CLIENT_ERROR_STATUS,
                "case {held}/{operation}: expected the client-error status, got {status} \
                 with {refused}"
            );
            assert_eq!(
                refused["__type"],
                json!("ConflictException"),
                "case {held}/{operation}: expected a conflict, got {refused}"
            );
            assert_eq!(
                refused["message"],
                json!("A cluster isn't in ACTIVE status."),
                "case {held}/{operation}: expected the service's own sentence, got {refused}"
            );
            assert_eq!(
                (
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (Some(cluster_id.as_str()), Some("cluster")),
                "case {held}/{operation}: expected the refusal to name the cluster, got \
                 {refused}"
            );
        }

        // Owe the cluster its settled status again, and the very next request serves the
        // same call: the gate is about the stored status, not about time passing.
        {
            let store = service_state(&*engine).expect("this service's own store");
            let mut state = store.lock();
            let cluster = state
                .cluster_mut("gated-cluster")
                .expect("the cluster the create persisted");
            cluster.status = STATUS_CREATING.to_string();
            cluster.pending = Some(PendingChange::Activate);
        }
        let (status, served) = call(
            &router,
            "CreateQueue",
            &create_queue("gated-cluster", "gated-queue", &[]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {held}: expected the same CreateQueue to be served once the cluster was \
             owed ACTIVE again, got {status} with {served}"
        );
    }
}

/// RULE: the call that starts a transition records the transient status the real service
/// reports, even where the operation answers an empty body — the delete has decided the
/// outcome and owes what the model declares for that resource.
#[tokio::test]
async fn a_delete_records_the_transient_status_and_the_change_it_owes() {
    let (router, engine) = emulator_with_engine();
    let (status, created) = call(
        &router,
        "CreateCluster",
        &create_cluster("transient-cluster", "SMALL"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCluster got {status} with {created}"
    );
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, created) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&cluster_id, "transient-nodes"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateComputeNodeGroup got {status} with {created}"
    );
    let (status, created) = call(
        &router,
        "CreateQueue",
        &create_queue(&cluster_id, "transient-queue", &["transient-nodes"]),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateQueue got {status} with {created}");

    // A queue's status enum declares no DELETED, so its delete owes the removal itself.
    let (status, deleted) = call(
        &router,
        "DeleteQueue",
        &json!({
            "clusterIdentifier": &cluster_id,
            "queueIdentifier": "transient-queue",
        }),
    )
    .await;
    assert_eq!(status, 200, "case DeleteQueue: got {status} with {deleted}");
    {
        let store = service_state(&*engine).expect("this service's own store");
        let state = store.lock();
        let cluster = state
            .cluster(&cluster_id)
            .expect("the cluster the create persisted");
        let queue = cluster
            .queue("transient-queue")
            .expect("the queue the delete moved");
        assert_eq!(
            (queue.status.as_str(), queue.pending),
            (STATUS_DELETING, Some(PendingChange::Remove)),
            "case DeleteQueue: expected the queue recorded DELETING and owed its removal, \
             got {:?}",
            (&queue.status, queue.pending)
        );
    }

    // A compute node group's enum DOES declare DELETED, so its delete owes that step first.
    let (status, deleted) = call(
        &router,
        "DeleteComputeNodeGroup",
        &json!({
            "clusterIdentifier": &cluster_id,
            "computeNodeGroupIdentifier": "transient-nodes",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case DeleteComputeNodeGroup: got {status} with {deleted}"
    );
    {
        let store = service_state(&*engine).expect("this service's own store");
        let state = store.lock();
        let cluster = state
            .cluster(&cluster_id)
            .expect("the cluster the create persisted");
        let group = cluster
            .compute_node_group("transient-nodes")
            .expect("the group the delete moved");
        assert_eq!(
            (group.status.as_str(), group.pending),
            (STATUS_DELETING, Some(PendingChange::MarkDeleted)),
            "case DeleteComputeNodeGroup: expected the group recorded DELETING and owed \
             DELETED, got {:?}",
            (&group.status, group.pending)
        );
    }

    // And the cluster, once the children no longer block it.
    let (status, deleted) = call(
        &router,
        "DeleteCluster",
        &json!({"clusterIdentifier": &cluster_id}),
    )
    .await;
    assert_eq!(status, 200, "case DeleteCluster: got {status} with {deleted}");
    {
        let store = service_state(&*engine).expect("this service's own store");
        let state = store.lock();
        let cluster = state
            .cluster(&cluster_id)
            .expect("the cluster the delete moved");
        assert_eq!(
            (cluster.status.as_str(), cluster.pending),
            (STATUS_DELETING, Some(PendingChange::Remove)),
            "case DeleteCluster: expected the cluster recorded DELETING and owed its \
             removal, got {:?}",
            (&cluster.status, cluster.pending)
        );
    }
}
