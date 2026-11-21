// SPDX-License-Identifier: CC0-1.0

//! Which compute node groups a stored PCS queue assigns jobs to, end to end over the
//! awsJson1_0 wire through the same router the emulator serves from, on the harness
//! [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a queue is stored only with an association list that NAMES at least one
//! compute node group — "You must associate 1 or more compute node groups with the queue"
//! (`CreateQueue`'s own documentation), the lower bound every path that writes that list
//! holds it to. `ComputeNodeGroupConfiguration` carries one, optional, member
//! (`smithy_query(query="shape", shape="ComputeNodeGroupConfiguration")`), so an entry that
//! leaves `computeNodeGroupId` out names no group and counts for nothing toward that bound:
//! a request whose configurations are absent, empty, or made only of such entries is refused
//! with the modelled `ValidationException` naming `computeNodeGroupConfigurations`, in the
//! standard constraint-violation framing, and persists nothing. A request naming one group
//! or several is served and stores the configurations exactly as it wrote them.
//!
//! Each test builds its own engine, so the state one test writes is invisible to the others,
//! and nothing here waits on the wall clock.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::MIN_QUEUE_ASSOCIATIONS;
use smithy_server_runtime::MultiServiceRouter;

use super::{call, create_cluster, create_compute_node_group, emulator};

/// The member a queue's associations travel in, in both request shapes that place them
/// (`smithy_query(query="shape", shape="CreateQueueRequest,UpdateQueueRequest")`).
const ASSOCIATIONS_MEMBER: &str = "computeNodeGroupConfigurations";

/// The constraint clause PCS states for an association list shorter than the fewest
/// compute node groups a stored queue assigns jobs to.
fn too_few_associations() -> String {
    format!("Member must have length greater than or equal to {MIN_QUEUE_ASSOCIATIONS}")
}

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

#[tokio::test]
async fn a_create_that_names_no_compute_node_group_is_refused_and_persists_nothing() {
    let router = emulator();
    let (cluster_id, _groups) = cluster_with_two_groups(&router).await;

    // Rows: every way a request can carry no association at all — the member absent
    // altogether, an empty list, and entries that name no group because the shape's one
    // member is optional, from a single such entry up to several.
    let rows: Vec<(&str, Option<Value>)> = vec![
        ("the member absent altogether", None),
        ("an empty list", Some(json!([]))),
        ("one entry naming no group", Some(json!([{}]))),
        ("three entries, none naming a group", Some(json!([{}, {}, {}]))),
    ];

    for (case, configurations) in rows {
        let mut request = json!({"clusterIdentifier": &cluster_id, "queueName": "work"});
        if let Some(configurations) = configurations {
            request[ASSOCIATIONS_MEMBER] = configurations;
        }
        let (status, refused) = call(&router, "CreateQueue", &request).await;
        assert_eq!(
            (status, refused["__type"].as_str()),
            (CLIENT_ERROR_STATUS, Some("ValidationException")),
            "case {case}: expected the modelled ValidationException, got {status} with \
             {refused}"
        );
        // The service's standard constraint-violation framing: how many violations were
        // detected, then the member path and the constraint its value failed.
        let expected = format!(
            "1 validation error(s) detected: Value at '{ASSOCIATIONS_MEMBER}' failed to \
             satisfy constraint: {}",
            too_few_associations()
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected {expected:?}, got {refused}"
        );
        // The same detail, structurally, for a client that does not parse the sentence.
        assert_eq!(
            refused["reason"].as_str(),
            Some("fieldValidationFailed"),
            "case {case}: expected the field-validation reason, got {refused}"
        );
        assert_eq!(
            refused["fieldList"],
            json!([{"name": ASSOCIATIONS_MEMBER, "message": too_few_associations()}]),
            "case {case}: expected one fieldList entry naming {ASSOCIATIONS_MEMBER}, got \
             {refused}"
        );
    }

    // Not one of the refused creates left a queue behind.
    let names = queue_names(&router, &cluster_id).await;
    assert!(
        names.is_empty(),
        "expected the refused creates to have persisted no queue, got {names:?}"
    );

    // And the name they all asked for is still free: the same call naming a group the
    // cluster holds is served.
    let (status, created) = call(
        &router,
        "CreateQueue",
        &json!({
            "clusterIdentifier": &cluster_id,
            "queueName": "work",
            "computeNodeGroupConfigurations": [{"computeNodeGroupId": "nodes-a"}],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the same create naming a group the cluster holds: got {status} with {created}"
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
