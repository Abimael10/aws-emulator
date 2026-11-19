// SPDX-License-Identifier: CC0-1.0

//! The delete ordering PCS holds a cluster's resources to, end to end over the awsJson1_0
//! wire (`POST /` with `X-Amz-Target: AWSParallelComputingService.<Operation>`) through
//! the same router the emulator serves from.
//!
//! RULE under test: a PCS resource can be deleted only once nothing of its cluster still
//! depends on it — "You must delete all queues associated with the compute node group
//! first" (`DeleteComputeNodeGroup`) and "You must delete all queues and compute node
//! groups associated with the cluster before you can delete the cluster"
//! (`DeleteCluster`) — and a delete attempted out of that order is refused with
//! `ConflictException` while the resource it names stays exactly as it was. A
//! delete made in order is accepted, and the removal it owes is materialised before the
//! next request is answered, so the read that follows it answers
//! `ResourceNotFoundException` and the listing has dropped it. Every refusal of a client
//! carries one status, the service's [`CLIENT_ERROR_STATUS`], and names its class in the
//! body.
//!
//! That ordering is TWO-sided: a create that would persist a reference to a resource whose
//! delete has already been accepted is refused the way one naming a resource the cluster
//! never held is, so no stored record can be left naming a resource the service is not
//! going to hold.
//!
//! The suites are tables. Their rows differ along what the rule generalises over: the form
//! of the identifier each call addresses a resource by (its generated id or its name, the
//! two forms the `*Identifier` patterns allow), the form a queue names a compute node group
//! by, and how many queues depend on one group — rather than the single shape the recorded
//! workflow happened to use.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use super::cluster_lifecycle::{call, emulator};

/// The region the shared harness signs every call for.
const REGION: &str = "us-east-1";

/// How a resource is addressed by a call: by the id PCS generated for it, or by the name
/// the create chose — the two forms `ClusterIdentifier`, `ComputeNodeGroupIdentifier` and
/// `QueueIdentifier` all allow
/// (`smithy_query(query="shape", shape="ComputeNodeGroupIdentifier")`:
/// `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,25})$`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// The generated `pcs_...` id.
    Id,
    /// The caller-chosen name.
    Name,
}

/// One created resource: its generated id and the name it was created under.
#[derive(Debug, Clone)]
struct Created {
    id: String,
    name: String,
}

impl Created {
    /// How `address` names this resource.
    fn as_addressed(&self, address: Address) -> &str {
        match address {
            Address::Id => &self.id,
            Address::Name => &self.name,
        }
    }
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// Create a cluster. The request that follows the create observes it ACTIVE.
async fn active_cluster(router: &MultiServiceRouter, case: &str, name: &str) -> Created {
    let body = json!({
        "clusterName": name,
        "scheduler": {"type": "SLURM", "version": "24.11"},
        "size": "SMALL",
        "networking": {
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
        },
    });
    let (status, created) = call(router, "CreateCluster", &body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateCluster must succeed, got {status} with {created}"
    );
    Created {
        id: created["cluster"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        name: name.to_string(),
    }
}

/// Create a compute node group in `cluster`. The request that follows the create observes
/// it ACTIVE.
async fn active_group(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    name: &str,
) -> Created {
    let body = json!({
        "clusterIdentifier": cluster,
        "computeNodeGroupName": name,
        "subnetIds": ["subnet-0123456789abcdef0"],
        "customLaunchTemplate": {"id": "lt-0123456789abcdef0", "version": "1"},
        "iamInstanceProfileArn": format!(
            "arn:aws:iam::{}:instance-profile/AWSPCS-demo",
            engine::EMULATOR_ACCOUNT_ID
        ),
        "scalingConfiguration": {"minInstanceCount": 0, "maxInstanceCount": 4},
        "instanceConfigs": [{"instanceType": "c6i.large"}],
    });
    let (status, created) = call(router, "CreateComputeNodeGroup", &body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateComputeNodeGroup must succeed, got {status} with {created}"
    );
    Created {
        id: created["computeNodeGroup"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        name: name.to_string(),
    }
}

/// Create a queue in `cluster` assigning jobs to each of `groups` (each written the way
/// that row addresses it). The request that follows the create observes it ACTIVE.
async fn active_queue(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    name: &str,
    groups: &[&str],
) -> Created {
    let body = json!({
        "clusterIdentifier": cluster,
        "queueName": name,
        "computeNodeGroupConfigurations": groups
            .iter()
            .map(|named| json!({"computeNodeGroupId": named}))
            .collect::<Vec<Value>>(),
    });
    let (status, created) = call(router, "CreateQueue", &body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateQueue must succeed, got {status} with {created}"
    );
    Created {
        id: created["queue"]["id"].as_str().unwrap_or_default().to_string(),
        name: name.to_string(),
    }
}

/// Assert `operation` on `body` is refused with `ConflictException` carrying `message`,
/// `resourceId` and `resourceType`.
async fn assert_conflict(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    message: &str,
    resource_id: &str,
    resource_type: &str,
) {
    let (status, refused) = call(router, operation, body).await;
    assert_eq!(
        status, CLIENT_ERROR_STATUS,
        "case {case}: expected {operation} {body} to be refused with the client-error \
         status, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ConflictException",
        "case {case}: expected ConflictException from {operation}, got {refused}"
    );
    assert_eq!(
        refused["message"].as_str(),
        Some(message),
        "case {case}: expected the service's own sentence from {operation}, got {refused}"
    );
    assert_eq!(
        (
            refused["resourceId"].as_str(),
            refused["resourceType"].as_str()
        ),
        (Some(resource_id), Some(resource_type)),
        "case {case}: expected {operation} to name the resource it refused, got {refused}"
    );
}

/// The sentence PCS answers a compute node group delete with while a queue still assigns
/// jobs to it.
const GROUP_STILL_REFERENCED: &str = "AWS PCS can't delete the compute node group you \
                                      specified because the cluster has associated queues.";

/// The sentence PCS answers a cluster delete with while the cluster still holds a queue or
/// a compute node group.
const CLUSTER_STILL_HAS_CHILDREN: &str =
    "AWS PCS can't delete the cluster you specified because it has associated queues or \
     compute node groups.";

/// RULE: out of order, each delete is refused with `ConflictException` and changes nothing;
/// in dependency order, each delete is accepted and the resource is gone — or, where the
/// status enum declares `DELETED`, reported `DELETED` once — from the next request on.
#[tokio::test]
async fn a_dependent_resource_blocks_the_delete_until_it_is_gone() {
    // Each row: how the delete calls address the resources, how the queue names the group,
    // and how many queues depend on that group.
    let rows: &[(&str, Address, Address, usize)] = &[
        ("addressed by id, one queue naming the group by id", Address::Id, Address::Id, 1),
        (
            "addressed by name, one queue naming the group by name",
            Address::Name,
            Address::Name,
            1,
        ),
        (
            "addressed by id, two queues naming the group by name",
            Address::Id,
            Address::Name,
            2,
        ),
    ];

    for (case, address, reference, queue_count) in rows {
        let router = emulator();
        let cluster = active_cluster(&router, case, "ordering-cluster").await;
        let group = active_group(&router, case, &cluster.id, "nodes").await;
        // A SECOND group nothing depends on, so the refusal below is shown to be keyed on
        // the queue's actual reference rather than on "a queue exists in this cluster".
        let unused = active_group(&router, case, &cluster.id, "spare-nodes").await;

        let mut queues = Vec::new();
        for index in 0..*queue_count {
            queues.push(
                active_queue(
                    &router,
                    case,
                    cluster.as_addressed(*address),
                    &format!("work-{index}"),
                    &[group.as_addressed(*reference)],
                )
                .await,
            );
        }

        let delete_group = json!({
            "clusterIdentifier": cluster.as_addressed(*address),
            "computeNodeGroupIdentifier": group.as_addressed(*address),
        });
        assert_conflict(
            &router,
            case,
            "DeleteComputeNodeGroup",
            &delete_group,
            GROUP_STILL_REFERENCED,
            &group.id,
            "computenodegroup",
        )
        .await;
        assert_conflict(
            &router,
            case,
            "DeleteCluster",
            &json!({"clusterIdentifier": cluster.as_addressed(*address)}),
            CLUSTER_STILL_HAS_CHILDREN,
            &cluster.id,
            "cluster",
        )
        .await;

        // The refused deletes changed nothing: both resources are still ACTIVE.
        for (operation, body, member) in [
            (
                "GetComputeNodeGroup",
                delete_group.clone(),
                "computeNodeGroup",
            ),
            (
                "GetCluster",
                json!({"clusterIdentifier": cluster.as_addressed(*address)}),
                "cluster",
            ),
        ] {
            let (status, read) = call(&router, operation, &body).await;
            assert_eq!(
                (status, read[member]["status"].as_str()),
                (200, Some("ACTIVE")),
                "case {case}: expected {operation} to still report ACTIVE after the refused \
                 delete, got {status} with {read}"
            );
        }

        // The group nothing depends on deletes while the queues are still there.
        let (status, deleted) = call(
            &router,
            "DeleteComputeNodeGroup",
            &json!({
                "clusterIdentifier": cluster.as_addressed(*address),
                "computeNodeGroupIdentifier": unused.as_addressed(*address),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the unreferenced group to delete, got {status} with \
             {deleted}"
        );

        // In order: every queue, then the group, then the cluster.
        for queue in &queues {
            let body = json!({
                "clusterIdentifier": cluster.as_addressed(*address),
                "queueIdentifier": queue.as_addressed(*address),
            });
            let (status, deleted) = call(&router, "DeleteQueue", &body).await;
            assert_eq!(
                status, 200,
                "case {case}: expected DeleteQueue {body} to succeed, got {status} with \
                 {deleted}"
            );
            // The removal the delete owed is materialised before the next request is
            // answered, so the read that follows it is already a miss.
            let (status, read) = call(&router, "GetQueue", &body).await;
            assert_eq!(
                (status, error_code(&read).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected the queue to be gone on the request after its \
                 delete, got {status} with {read}"
            );
            // Deleting it again names a queue the cluster no longer holds.
            let (status, again) = call(&router, "DeleteQueue", &body).await;
            assert_eq!(
                (status, error_code(&again).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected a second DeleteQueue to report the queue gone, got \
                 {status} with {again}"
            );
        }

        let (status, listed) = call(
            &router,
            "ListQueues",
            &json!({"clusterIdentifier": cluster.as_addressed(*address)}),
        )
        .await;
        assert_eq!(
            (status, listed["queues"].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {case}: expected no queue left in the cluster, got {status} with {listed}"
        );

        let (status, deleted) = call(&router, "DeleteComputeNodeGroup", &delete_group).await;
        assert_eq!(
            status, 200,
            "case {case}: expected DeleteComputeNodeGroup to succeed once no queue depends \
             on it, got {status} with {deleted}"
        );
        // The group's delete has come due by the next request: the one resource of the
        // three whose status enum declares DELETED is reported DELETED before it is
        // forgotten.
        let (status, read) = call(&router, "GetComputeNodeGroup", &delete_group).await;
        assert_eq!(
            (status, read["computeNodeGroup"]["status"].as_str()),
            (200, Some("DELETED")),
            "case {case}: expected the group to be reported DELETED on the request after \
             its delete, got {status} with {read}"
        );

        let (status, gone) = call(&router, "GetComputeNodeGroup", &delete_group).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the group to be gone once its removal came due, got \
             {status} with {gone}"
        );
        let (status, listed) = call(
            &router,
            "ListComputeNodeGroups",
            &json!({"clusterIdentifier": cluster.as_addressed(*address)}),
        )
        .await;
        assert_eq!(
            (
                status,
                listed["computeNodeGroups"].as_array().map(Vec::len)
            ),
            (200, Some(0)),
            "case {case}: expected no compute node group left in the cluster, got {status} \
             with {listed}"
        );

        let cluster_body = json!({"clusterIdentifier": cluster.as_addressed(*address)});
        let (status, deleted) = call(&router, "DeleteCluster", &cluster_body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected DeleteCluster to succeed once the cluster is empty, got \
             {status} with {deleted}"
        );
        let (status, gone) = call(&router, "GetCluster", &cluster_body).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the cluster to be gone on the request after its delete, \
             got {status} with {gone}"
        );
    }
}

/// RULE: the delete order is about DEPENDENTS, not about waiting — a caller that deletes a
/// queue, then its compute node group, then the cluster back to back, with no wait between
/// the calls, is served every time, and the clock then removes all three.
#[tokio::test]
async fn deleting_the_hierarchy_back_to_back_is_served() {
    let router = emulator();
    let case = "back-to-back ordered deletes";
    let cluster = active_cluster(&router, case, "rapid-cluster").await;
    let group = active_group(&router, case, &cluster.id, "nodes").await;
    let queue = active_queue(&router, case, &cluster.id, "work", &[&group.id]).await;

    // Each row: the operation and the request, issued one after the other with no wait.
    let rows: Vec<(&str, Value)> = vec![
        (
            "DeleteQueue",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "queueIdentifier": queue.id.clone(),
            }),
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "computeNodeGroupIdentifier": group.id.clone(),
            }),
        ),
        (
            "DeleteCluster",
            json!({"clusterIdentifier": cluster.id.clone()}),
        ),
    ];
    for (operation, body) in &rows {
        let (status, deleted) = call(&router, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {operation} {body} to be served straight after the \
             previous delete, got {status} with {deleted}"
        );
    }

    // All three removals were owed from the deletes that started them, so every read of
    // them is a miss. The CLUSTER is gone too, and every read here names it by the
    // GENERATED ID it was minted with, so each is the cluster's own not-found rather than a
    // refusal about the caller.
    for (operation, body, expected_code) in [
        (
            "GetQueue",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "queueIdentifier": queue.id.clone(),
            }),
            "ResourceNotFoundException",
        ),
        (
            "GetComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "computeNodeGroupIdentifier": group.id.clone(),
            }),
            "ResourceNotFoundException",
        ),
        (
            "GetCluster",
            json!({"clusterIdentifier": cluster.id.clone()}),
            "ResourceNotFoundException",
        ),
    ] {
        let (status, gone) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, expected_code),
            "case {case}: expected {operation} to report the resource gone, got {status} \
             with {gone}"
        );
    }
}

/// RULE: a delete names a cluster and a resource inside it, so a resource INSIDE a cluster
/// that holds no such resource is `ResourceNotFoundException` naming the resource the
/// request could not resolve: `resourceId` is the identifier as supplied and `resourceType`
/// the KIND of identifier that did not resolve. (A delete naming no CLUSTER is refused
/// about the caller instead — see `tests/all/cluster_scoped_refusals.rs`.)
#[tokio::test]
async fn a_delete_of_a_resource_nothing_holds_is_not_found() {
    let router = emulator();
    let case = "a delete of what nothing holds";
    let cluster = active_cluster(&router, case, "lonely-cluster").await;

    // Each row: the operation, the request, the identifier that does not resolve, and the
    // `resourceType` the refusal reports for it. Every row names a cluster that exists, so
    // the refusal is about the resource inside it.
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "DeleteQueue",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "queueIdentifier": "no-such-queue",
            }),
            "no-such-queue",
            "Queue ID",
        ),
        (
            "DeleteQueue",
            json!({
                "clusterIdentifier": cluster.name.clone(),
                "queueIdentifier": "pcs_00000000000000000000",
            }),
            "pcs_00000000000000000000",
            "Queue ID",
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster.id.clone(),
                "computeNodeGroupIdentifier": "no-such-group",
            }),
            "no-such-group",
            "Compute Node Group ID",
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster.name.clone(),
                "computeNodeGroupIdentifier": "pcs_11111111111111111111",
            }),
            "pcs_11111111111111111111",
            "Compute Node Group ID",
        ),
    ];

    for (operation, body, identifier, resource_type) in rows {
        let (status, refused) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected {operation} {body} to answer the client-error status, got \
             {status} with {refused}"
        );
        // The refusal names the identifier it could not resolve and the kind of
        // identifier that was.
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(identifier), Some(resource_type)),
            "case {case}: expected {operation} to name the identifier it could not \
             resolve, got {refused}"
        );
    }
}

/// The sentence PCS answers a request naming a compute node group it cannot find with
/// (`crate::state::not_found`'s own fixed sentence for the kind).
const GROUP_NOT_FOUND: &str = "AWS PCS couldn't find the compute node group you specified.";

/// RULE: a create stores a reference to another resource only when that resource is one the
/// service is still going to hold, so a `computeNodeGroupConfigurations` entry naming a
/// compute node group whose delete has already been accepted is refused exactly as one
/// naming a group the cluster never held is — `ResourceNotFoundException` for the compute
/// node group, and nothing persisted.
///
/// The group a row names has had its delete accepted by the previous request, so the settle
/// pass at the top of the create reports it `DELETED` and the request after that forgets its
/// record: were the create served, the stored queue would name a group the cluster does not
/// hold, for the rest of the queue's life and with no later call able to repair it.
#[tokio::test]
async fn a_queue_cannot_reference_a_compute_node_group_whose_delete_was_accepted() {
    // Each row: how the create addresses its cluster, how a configuration names a group,
    // how many configurations the request carries, and where in that list the group on its
    // way out sits — every other entry names an ACTIVE group of the same cluster.
    let rows: &[(&str, Address, Address, usize, usize)] = &[
        (
            "cluster by id, group by id, the only configuration",
            Address::Id,
            Address::Id,
            1,
            0,
        ),
        (
            "cluster by name, group by name, the only configuration",
            Address::Name,
            Address::Name,
            1,
            0,
        ),
        (
            "cluster by id, group by name, last of two configurations",
            Address::Id,
            Address::Name,
            2,
            1,
        ),
        (
            "cluster by name, group by id, first of three configurations",
            Address::Name,
            Address::Id,
            3,
            0,
        ),
    ];

    for (case, cluster_address, reference, configurations, position) in rows {
        let router = emulator();
        let cluster = active_cluster(&router, case, "dangling-cluster").await;
        let doomed = active_group(&router, case, &cluster.id, "nodes").await;
        let mut spares = Vec::new();
        for index in 1..*configurations {
            spares.push(active_group(&router, case, &cluster.id, &format!("spare-{index}")).await);
        }

        // No queue assigns jobs to it, so the group's delete is accepted and the service
        // owes its removal from here on. `DeleteComputeNodeGroup` answers an empty body;
        // the group it left `DELETING` is what the next request's settle pass reports
        // `DELETED` before the record is forgotten.
        let delete_group = json!({
            "clusterIdentifier": cluster.id.clone(),
            "computeNodeGroupIdentifier": doomed.id.clone(),
        });
        let (status, deleted) = call(&router, "DeleteComputeNodeGroup", &delete_group).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the group's delete to be accepted, got {status} with \
             {deleted}"
        );

        let mut named: Vec<String> = spares
            .iter()
            .map(|spare| spare.as_addressed(*reference).to_string())
            .collect();
        named.insert(*position, doomed.as_addressed(*reference).to_string());
        let create_queue = json!({
            "clusterIdentifier": cluster.as_addressed(*cluster_address),
            "queueName": "work",
            "computeNodeGroupConfigurations": named
                .iter()
                .map(|named| json!({"computeNodeGroupId": named}))
                .collect::<Vec<Value>>(),
        });
        let (status, refused) = call(&router, "CreateQueue", &create_queue).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected CreateQueue {create_queue} to be refused for the group \
             on its way out, got {status} with {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(GROUP_NOT_FOUND),
            "case {case}: expected the service's own sentence for a compute node group it \
             cannot find, got {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (
                Some(doomed.as_addressed(*reference)),
                Some("Compute Node Group ID")
            ),
            "case {case}: expected the refusal to name the configuration's identifier, got \
             {refused}"
        );

        // An identifier the cluster NEVER held earns the same refusal, which is what makes
        // "a group whose removal is owed" and "no such group" one answer.
        let unknown = json!({
            "clusterIdentifier": cluster.as_addressed(*cluster_address),
            "queueName": "work",
            "computeNodeGroupConfigurations": [{"computeNodeGroupId": "no-such-group"}],
        });
        let (status, unknown_refusal) = call(&router, "CreateQueue", &unknown).await;
        assert_eq!(
            (
                status,
                error_code(&unknown_refusal).as_str(),
                unknown_refusal["message"].as_str(),
                unknown_refusal["resourceType"].as_str()
            ),
            (
                CLIENT_ERROR_STATUS,
                "ResourceNotFoundException",
                Some(GROUP_NOT_FOUND),
                Some("Compute Node Group ID")
            ),
            "case {case}: expected a configuration naming an unheld group to be refused the \
             same way, got {status} with {unknown_refusal}"
        );

        // Nothing was persisted, so no stored queue can name the group once its record is
        // forgotten.
        let in_cluster = json!({"clusterIdentifier": cluster.id.clone()});
        let (status, listed) = call(&router, "ListQueues", &in_cluster).await;
        assert_eq!(
            (status, listed["queues"].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {case}: expected the refused creates to have stored no queue, got {status} \
             with {listed}"
        );

        // The group's removal has come due meanwhile: the reference the refused create would
        // have stored would now resolve to nothing.
        let (status, gone) = call(&router, "GetComputeNodeGroup", &delete_group).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the group to be gone once its removal came due, got \
             {status} with {gone}"
        );

        // The same create against the cluster's ACTIVE groups alone is served and reads
        // back, so the refusal is keyed on the accepted delete rather than on naming a
        // group at all.
        if !spares.is_empty() {
            let still_held: Vec<&str> = spares
                .iter()
                .map(|spare| spare.as_addressed(*reference))
                .collect();
            let queue = active_queue(
                &router,
                case,
                cluster.as_addressed(*cluster_address),
                "work",
                &still_held,
            )
            .await;
            let (status, read) = call(
                &router,
                "GetQueue",
                &json!({
                    "clusterIdentifier": cluster.id.clone(),
                    "queueIdentifier": queue.id.clone(),
                }),
            )
            .await;
            let stored: Vec<&str> = read["queue"]["computeNodeGroupConfigurations"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["computeNodeGroupId"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                (status, stored),
                (200, still_held),
                "case {case}: expected the stored queue to name the groups the create \
                 carried, got {status} with {read}"
            );
        }
    }
}

/// RULE: a compute node group listing reports the groups of its own cluster in creation
/// order, one page at a time, and resumes exactly where a page stopped.
#[tokio::test]
async fn the_compute_node_group_listing_pages_in_creation_order() {
    let router = emulator();
    let case = "paging a cluster's compute node groups";
    let cluster = active_cluster(&router, case, "listing-cluster").await;
    let mut created = Vec::new();
    for index in 0..3 {
        created.push(
            active_group(&router, case, &cluster.id, &format!("nodes-{index}"))
                .await
                .id,
        );
    }

    // Each row: the page size asked for.
    for page_size in [1, 2, 3, 100] {
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({
                "clusterIdentifier": cluster.id.clone(),
                "maxResults": page_size,
            });
            if let Some(resume) = &token {
                body["nextToken"] = json!(resume);
            }
            let (status, page) = call(&router, "ListComputeNodeGroups", &body).await;
            assert_eq!(
                status, 200,
                "case {case} at maxResults={page_size}: expected 200, got {status} with \
                 {page}"
            );
            let groups = page["computeNodeGroups"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                groups.len() <= page_size,
                "case {case} at maxResults={page_size}: expected at most {page_size} \
                 groups in a page, got {page}"
            );
            for group in &groups {
                assert_eq!(
                    group["clusterId"].as_str(),
                    Some(cluster.id.as_str()),
                    "case {case}: expected every listed group to belong to this cluster, \
                     got {group}"
                );
                assert!(
                    group["arn"]
                        .as_str()
                        .is_some_and(|arn| arn.contains(&format!(":pcs:{REGION}:"))),
                    "case {case}: expected the group ARN to carry the signed region, got \
                     {group}"
                );
                collected.push(group["id"].as_str().unwrap_or_default().to_string());
            }
            match page["nextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
            assert!(
                collected.len() <= created.len(),
                "case {case} at maxResults={page_size}: paging reported more groups than \
                 the cluster holds: {collected:?}"
            );
        }
        assert_eq!(
            collected, created,
            "case {case} at maxResults={page_size}: expected the groups in creation order"
        );
    }

    // A token no listing of this cluster issued names no page.
    let (status, refused) = call(
        &router,
        "ListComputeNodeGroups",
        &json!({
            "clusterIdentifier": cluster.id.clone(),
            "nextToken": "not-a-token-this-listing-issued",
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (CLIENT_ERROR_STATUS, "ValidationException"),
        "case {case}: expected an unissued token to be refused, got {status} with {refused}"
    );
}
