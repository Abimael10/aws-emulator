// SPDX-License-Identifier: CC0-1.0

//! How PCS WORDS a refusal of a create that would take a store past a record-count
//! ceiling, end to end over the awsJson1_0 wire through the same router the emulator
//! serves from, on the harness [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a refusal states the service's own fixed sentence for the thing it
//! refuses and carries neither an emulator-internal value nor an ARN wire token in that
//! sentence — the request's and the service's own values travel in the error's modelled
//! members. For a reached record count that sentence is the one
//! `ServiceQuotaExceededException`'s own documentation states for it
//! (`smithy_query(query="doc", shape="ServiceQuotaExceededException")`): clusters and
//! queues are counted for the account, compute node groups for the cluster they belong
//! to. The ceiling itself is this emulator's own number, not a quota AWS publishes, so no
//! sentence carries it.
//!
//! The rows span every record-count ceiling this service keeps — the one held across the
//! account and the two held per cluster — and therefore both documented sentences and all
//! three resource kinds a create mints. That the ceilings are REACHED where they are
//! (which create earns the refusal) is `crate::cluster_lifecycle`'s; what the refusal then
//! SAYS is here.
//!
//! Each test builds its own engine, so the state one test writes is invisible to the
//! others, and nothing here waits on the wall clock.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::{
    MAX_CLUSTERS, MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER, MAX_QUEUES_PER_CLUSTER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{
    call, create_cluster, create_compute_node_group, create_queue, emulator,
};

/// The modelled error a reached ceiling is refused with.
const QUOTA_ERROR: &str = "ServiceQuotaExceededException";

/// The sentence the service states when a count held across the ACCOUNT is reached: the
/// first of `ServiceQuotaExceededException`'s own documented examples
/// (`smithy_query(query="doc", shape="ServiceQuotaExceededException")`).
const ACCOUNT_CEILING_SENTENCE: &str =
    "The max number of clusters or queues has been reached for the account.";

/// The sentence the service states when a cluster's compute node group count is reached:
/// the second of that same error's documented examples.
const CLUSTER_CEILING_SENTENCE: &str =
    "The max number of compute node groups has been reached for the associated cluster.";

/// The `resourceType` this service's errors carry for a compute node group — the segment
/// that names the kind in a PCS ARN — which is a wire token and therefore belongs in that
/// member and in no sentence.
const COMPUTE_NODE_GROUP_TOKEN: &str = "computenodegroup";

/// The `serviceCode` the modelled error carries (`@required`): this service's own code.
const SERVICE_CODE: &str = "pcs";

/// Every assertion a reached-ceiling refusal must satisfy, over one row.
///
/// `expected_resource_type` is the `resourceType` member the refusal reports the kind of
/// resource in; the SENTENCE names no kind by that token, whichever row it is.
fn assert_quota_refusal(
    case: &str,
    status: u16,
    body: &Value,
    expected_message: &str,
    expected_resource_type: &str,
) {
    assert_eq!(
        status, CLIENT_ERROR_STATUS,
        "case {case}: expected the client-error status {CLIENT_ERROR_STATUS}, got {status} {body}"
    );
    assert_eq!(
        body["__type"].as_str(),
        Some(QUOTA_ERROR),
        "case {case}: expected {QUOTA_ERROR}, got {body}"
    );

    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, expected_message,
        "case {case}: expected the service's own sentence {expected_message:?}, got {message:?}"
    );
    assert!(
        !message.chars().any(|character| character.is_ascii_digit()),
        "case {case}: expected a sentence carrying no ceiling of the emulator's, \
         got {message:?}"
    );
    assert!(
        !message.contains(COMPUTE_NODE_GROUP_TOKEN),
        "case {case}: expected no ARN wire token in the sentence, got {message:?}"
    );

    assert_eq!(
        body["resourceType"].as_str(),
        Some(expected_resource_type),
        "case {case}: expected the kind of resource in resourceType, got {body}"
    );
    assert_eq!(
        body["serviceCode"].as_str(),
        Some(SERVICE_CODE),
        "case {case}: expected serviceCode {SERVICE_CODE:?}, got {body}"
    );
    assert!(
        body["quotaCode"].is_null(),
        "case {case}: expected no quotaCode to be invented, got {body}"
    );
}

/// Create a cluster and answer its id. The request that follows a create observes the
/// resource settled, so there is nothing to wait for.
async fn active_cluster(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(router, "CreateCluster", &create_cluster(name, "SMALL")).await;
    assert_eq!(status, 200, "setup: CreateCluster {name}: got {created}");
    created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_cluster_create_past_the_account_ceiling_states_the_services_own_sentence() {
    let router = emulator();
    for index in 0..MAX_CLUSTERS {
        let name = format!("quota-cluster-{index}");
        let (status, body) = call(&router, "CreateCluster", &create_cluster(&name, "SMALL")).await;
        assert_eq!(
            status, 200,
            "setup: cluster {index} of {MAX_CLUSTERS}: expected 200, got {status} {body}"
        );
    }

    let (status, refused) = call(
        &router,
        "CreateCluster",
        &create_cluster("quota-cluster-over", "SMALL"),
    )
    .await;
    assert_quota_refusal(
        "one cluster past the account ceiling",
        status,
        &refused,
        ACCOUNT_CEILING_SENTENCE,
        // The cluster kind's `resourceType`, the segment naming it in a PCS ARN.
        "cluster",
    );
}

#[tokio::test]
async fn a_compute_node_group_create_past_its_clusters_ceiling_states_the_services_own_sentence() {
    let router = emulator();
    let cluster = active_cluster(&router, "quota-group-host").await;
    for index in 0..MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER {
        let name = format!("quota-cng-{index}");
        let (status, body) = call(
            &router,
            "CreateComputeNodeGroup",
            &create_compute_node_group(&cluster, &name),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: compute node group {index} of {MAX_COMPUTE_NODE_GROUPS_PER_CLUSTER}: \
             expected 200, got {status} {body}"
        );
    }

    let (status, refused) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&cluster, "quota-cng-over"),
    )
    .await;
    assert_quota_refusal(
        "one compute node group past its cluster's ceiling",
        status,
        &refused,
        CLUSTER_CEILING_SENTENCE,
        COMPUTE_NODE_GROUP_TOKEN,
    );

    // The sentence says the count is the ASSOCIATED cluster's, so another cluster still
    // takes a group of its own.
    let elsewhere = active_cluster(&router, "quota-group-neighbour").await;
    let (status, body) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&elsewhere, "quota-cng-elsewhere"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a group in another cluster: expected the ceiling to be that cluster's own, \
         got {status} {body}"
    );
}

#[tokio::test]
async fn a_queue_create_past_its_clusters_ceiling_states_the_services_own_sentence() {
    let router = emulator();
    let cluster = active_cluster(&router, "quota-queue-host").await;
    let (status, group) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&cluster, "quota-queue-nodes"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateComputeNodeGroup: got {status} {group}"
    );

    for index in 0..MAX_QUEUES_PER_CLUSTER {
        let name = format!("quota-queue-{index}");
        let (status, body) = call(
            &router,
            "CreateQueue",
            &create_queue(&cluster, &name, &["quota-queue-nodes"]),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: queue {index} of {MAX_QUEUES_PER_CLUSTER}: expected 200, got {status} {body}"
        );
    }

    let (status, refused) = call(
        &router,
        "CreateQueue",
        &create_queue(&cluster, "quota-queue-over", &["quota-queue-nodes"]),
    )
    .await;
    // A queue is held per cluster here, yet the service states the ACCOUNT sentence for
    // it: that sentence is the one its own documentation gives for a reached queue count.
    assert_quota_refusal(
        "one queue past its cluster's ceiling",
        status,
        &refused,
        ACCOUNT_CEILING_SENTENCE,
        // The queue kind's `resourceType`, the segment naming it in a PCS ARN.
        "queue",
    );

    // Nothing of the refused create was persisted: the queue count is still the ceiling.
    let (status, queues) = call(
        &router,
        "ListQueues",
        &json!({"clusterIdentifier": &cluster, "maxResults": 100}),
    )
    .await;
    assert_eq!(status, 200, "case ListQueues after the refusal: got {queues}");
    assert_eq!(
        queues["queues"].as_array().map(Vec::len),
        Some(MAX_QUEUES_PER_CLUSTER),
        "case ListQueues after the refusal: expected the refused create to have persisted \
         nothing, got {queues}"
    );
}
