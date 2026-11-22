// SPDX-License-Identifier: CC0-1.0

//! Which compute node groups a stored PCS queue assigns jobs to, end to end over the
//! awsJson1_0 wire through the same router the emulator serves from, on the harness
//! [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a queue stores the associations its request wrote and HOW MANY groups
//! they name is never itself a ground for refusal — `computeNodeGroupConfigurations` is an
//! optional member of an unbounded list whose `ComputeNodeGroupConfiguration` carries one
//! optional member (`smithy_query(query="shape",
//! shape="CreateQueueRequest,ComputeNodeGroupConfigurationList,ComputeNodeGroupConfiguration")`),
//! so a create that names no group at all is served and stores exactly that, and `UpdateQueue`
//! is what associates a group later. The only refusal that member can earn is naming a
//! compute node group the cluster does not hold, and the cluster the create is scoped to is
//! reported as not found however few groups the request names.
//!
//! Each test builds its own engine, so the state one test writes is invisible to the others,
//! and nothing here waits on the wall clock.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use super::{call, create_cluster, create_compute_node_group, emulator};

/// The member a queue's associations travel in, in both request shapes that place them
/// (`smithy_query(query="shape", shape="CreateQueueRequest,UpdateQueueRequest")`).
const ASSOCIATIONS_MEMBER: &str = "computeNodeGroupConfigurations";

/// The sentence PCS states a cluster it cannot find with: fixed text throughout, echoing no
/// value the caller sent.
const CLUSTER_NOT_FOUND: &str = "AWS PCS couldn't find the cluster you specified.";

/// A cluster holding two compute node groups, as `(cluster id, [(group name, group id)])`.
/// Every resource is settled by the request that follows its own create, so there is
/// nothing to wait for.
async fn cluster_with_two_groups(router: &MultiServiceRouter) -> (String, Vec<(String, String)>) {
    let (status, created) = call(
        router,
        "CreateCluster",
        &create_cluster("queue-host", "SMALL"),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateCluster: got {status} {created}");
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let mut groups = Vec::new();
    for name in ["nodes-a", "nodes-b"] {
        let (status, group) = call(
            router,
            "CreateComputeNodeGroup",
            &create_compute_node_group(&cluster_id, name),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateComputeNodeGroup {name}: got {status} {group}"
        );
        groups.push((
            name.to_string(),
            group["computeNodeGroup"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    (cluster_id, groups)
}

/// The queues `ListQueues` reports for `cluster`, by name, in the order it lists them.
async fn queue_names(router: &MultiServiceRouter, cluster: &str) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListQueues",
        &json!({"clusterIdentifier": cluster, "maxResults": 100}),
    )
    .await;
    assert_eq!(status, 200, "ListQueues must answer 200, got {listed}");
    listed["queues"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a create that names no compute node group is SERVED and stores the association list
/// it wrote, whatever form "no group" took.
#[tokio::test]
async fn a_create_that_names_no_compute_node_group_is_served_and_stores_what_it_wrote() {
    let router = emulator();
    let (cluster_id, _groups) = cluster_with_two_groups(&router).await;

    // Rows: every way a request can carry no association at all — the member absent
    // altogether, an empty list, and entries that name no group because the shape's one
    // member is optional, from a single such entry up to several.
    let rows: Vec<(&str, &str, Option<Value>)> = vec![
        ("the member absent altogether", "absent", None),
        ("an empty list", "empty", Some(json!([]))),
        ("one entry naming no group", "unnamed-one", Some(json!([{}]))),
        (
            "three entries, none naming a group",
            "unnamed-three",
            Some(json!([{}, {}, {}])),
        ),
    ];

    for (case, queue, configurations) in &rows {
        let mut request = json!({"clusterIdentifier": &cluster_id, "queueName": queue});
        if let Some(configurations) = configurations {
            request[ASSOCIATIONS_MEMBER] = configurations.clone();
        }
        let (status, created) = call(&router, "CreateQueue", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the create served, got {status} with {created}"
        );
        // An absent member stores the empty list; everything else stores exactly what the
        // request wrote, entries naming no group included.
        let stored = configurations.clone().unwrap_or_else(|| json!([]));
        assert_eq!(
            created["queue"][ASSOCIATIONS_MEMBER], stored,
            "case {case}: expected the configurations the request wrote, got {created}"
        );
        let (status, read) = call(
            &router,
            "GetQueue",
            &json!({"clusterIdentifier": &cluster_id, "queueIdentifier": queue}),
        )
        .await;
        assert_eq!(status, 200, "case {case}: GetQueue got {status} with {read}");
        assert_eq!(
            read["queue"][ASSOCIATIONS_MEMBER], stored,
            "case {case}: expected the stored configurations read back, got {read}"
        );
    }

    let expected: Vec<String> = rows
        .iter()
        .map(|(_, queue, _)| (*queue).to_string())
        .collect();
    let names = queue_names(&router, &cluster_id).await;
    assert_eq!(
        names, expected,
        "expected every served create to have persisted its queue, got {names:?}"
    );
}

/// RULE: what a create naming no group IS refused about is the cluster it is scoped to —
/// a `clusterIdentifier` in the generated-id form that resolves to nothing earns the
/// modelled cluster not-found, however few groups the request names, and persists nothing.
#[tokio::test]
async fn a_create_on_an_absent_cluster_reports_the_cluster_whatever_it_associates() {
    let router = emulator();
    let absent = format!("pcs_{}", "2".repeat(20));

    // Rows: no associations at all, in each form, and one naming a group — the refusal
    // follows the cluster identifier and not the association list.
    let rows: Vec<(&str, Option<Value>)> = vec![
        ("the member absent altogether", None),
        ("an empty list", Some(json!([]))),
        ("one entry naming no group", Some(json!([{}]))),
        (
            "one entry naming a group",
            Some(json!([{"computeNodeGroupId": "nodes"}])),
        ),
    ];

    for (case, configurations) in rows {
        let mut request = json!({"clusterIdentifier": &absent, "queueName": "work"});
        if let Some(configurations) = configurations {
            request[ASSOCIATIONS_MEMBER] = configurations;
        }
        let (status, refused) = call(&router, "CreateQueue", &request).await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ResourceNotFoundException")),
            "case {case}: expected the cluster not-found, got {status} with {refused}"
        );
        assert_eq!(
            (
                refused["message"].as_str(),
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (
                Some(CLUSTER_NOT_FOUND),
                Some(absent.as_str()),
                Some("Cluster ID")
            ),
            "case {case}: expected the fixed cluster sentence with the identifier as \
             supplied, got {refused}"
        );
    }

    // Nothing was stored: the identifier still resolves to no cluster.
    let (status, refused) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": &absent}),
    )
    .await;
    assert_eq!(
        (status, refused["__type"].as_str()),
        (CLIENT_ERROR_STATUS, Some("ResourceNotFoundException")),
        "case GetCluster after the refused creates: got {status} with {refused}"
    );
}

#[tokio::test]
async fn a_create_naming_a_compute_node_group_stores_the_associations_it_was_given() {
    let router = emulator();
    let (cluster_id, groups) = cluster_with_two_groups(&router).await;
    let (first_name, first_id) = groups[0].clone();
    let (second_name, _second_id) = groups[1].clone();

    // Rows: the two forms a configuration can name a group in (a group NAME and the
    // generated group id), how many groups one queue associates, and an entry naming no
    // group BESIDE one that does — what the bound counts is the configurations that name a
    // group.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "one group, by name",
            "by-name",
            json!([{"computeNodeGroupId": &first_name}]),
        ),
        (
            "one group, by its generated id",
            "by-id",
            json!([{"computeNodeGroupId": &first_id}]),
        ),
        (
            "two groups",
            "by-both",
            json!([
                {"computeNodeGroupId": &first_name},
                {"computeNodeGroupId": &second_name},
            ]),
        ),
        (
            "one group beside an entry naming none",
            "with-unnamed",
            json!([{}, {"computeNodeGroupId": &first_id}]),
        ),
    ];

    for (case, queue, configurations) in &rows {
        let (status, created) = call(
            &router,
            "CreateQueue",
            &json!({
                "clusterIdentifier": &cluster_id,
                "queueName": queue,
                "computeNodeGroupConfigurations": configurations,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the create served, got {status} with {created}"
        );
        assert_eq!(
            &created["queue"][ASSOCIATIONS_MEMBER], configurations,
            "case {case}: expected the configurations the request wrote, got {created}"
        );

        // Read back through the queue's own reader: a create and a later read cannot differ.
        let (status, read) = call(
            &router,
            "GetQueue",
            &json!({"clusterIdentifier": &cluster_id, "queueIdentifier": queue}),
        )
        .await;
        assert_eq!(status, 200, "case {case}: GetQueue got {status} with {read}");
        assert_eq!(
            &read["queue"][ASSOCIATIONS_MEMBER], configurations,
            "case {case}: expected the stored configurations read back, got {read}"
        );
    }

    let expected: Vec<String> = rows
        .iter()
        .map(|(_, queue, _)| (*queue).to_string())
        .collect();
    let names = queue_names(&router, &cluster_id).await;
    assert_eq!(
        names, expected,
        "expected every served create to have persisted its queue, got {names:?}"
    );
}
