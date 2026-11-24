// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `@idempotencyToken` of `DeleteCluster` and
//! `DeleteComputeNodeGroup`, driven over the awsJson1_0 wire through the same router the
//! emulator serves from, on the harness [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: both operations are `@idempotent` and their `clientToken` is an
//! `@idempotencyToken`, so "if the original request completes successfully, the subsequent
//! retries with the same client token return the result from the original successful
//! request and they have no additional effect"
//! (`smithy_query(query="doc",shape="DeleteClusterRequest",member="clientToken")`, the
//! same sentence on `DeleteComputeNodeGroupRequest$clientToken`). A retry presenting the
//! token an ACCEPTED delete used is therefore answered with that call's empty success,
//! whichever form it addresses the resource by, and changes nothing — while a request that
//! presents no token, or another token, is a delete of its own and is answered on the
//! state it finds, and a token an accepted delete used for ANOTHER resource conflicts
//! rather than silently skipping that resource's delete.
//!
//! The suites are tables. Their rows differ along what the rule generalises over: the form
//! each call addresses the cluster and the group by (the generated id or the name, the two
//! forms the `*Identifier` patterns allow), the length of the token itself at both of
//! `SBClientToken`'s published bounds, and how far the service has carried the delete it
//! owes by the time the second call arrives.
//!
//! Nothing here waits on the wall clock: a transition this service owes is materialised
//! before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{call, create_cluster, create_compute_node_group, emulator};

/// How a call addresses a resource: by the id PCS generated for it, or by the name its
/// create chose — the two forms `ClusterIdentifier` and `ComputeNodeGroupIdentifier` both
/// allow (`smithy_query(query="shape",shape="ClusterIdentifier")`:
/// `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$`).
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

/// A token at the SHORTEST length the model admits (`SBClientToken` is
/// `@length(min: 8, max: 100)`).
fn shortest_token() -> String {
    "tok-0001".to_string()
}

/// A token at the LONGEST length the model admits (`SBClientToken`'s `@length` max of
/// 100), so the two published bounds of the member are both exercised.
fn longest_token() -> String {
    let token = format!("{:A<100}", "tok-");
    assert_eq!(
        token.len(),
        100,
        "the longest token must be exactly the modelled maximum, got {token:?}"
    );
    token
}

/// A `DeleteCluster` request for `cluster`, carrying `token` when the case presents one.
fn delete_cluster(cluster: &str, token: Option<&str>) -> Value {
    let mut body = json!({"clusterIdentifier": cluster});
    if let Some(token) = token {
        body["clientToken"] = json!(token);
    }
    body
}

/// A `DeleteComputeNodeGroup` request for `group` of `cluster`, carrying `token` when the
/// case presents one.
fn delete_group(cluster: &str, group: &str, token: Option<&str>) -> Value {
    let mut body = json!({
        "clusterIdentifier": cluster,
        "computeNodeGroupIdentifier": group,
    });
    if let Some(token) = token {
        body["clientToken"] = json!(token);
    }
    body
}

/// Create a cluster and settle it, handing back what it was created as. A created resource
/// is reported `CREATING` by the call that created it and is `ACTIVE` from the next request
/// on, so the read below is what settles it rather than any wait.
async fn active_cluster(router: &MultiServiceRouter, case: &str, name: &str) -> Created {
    let (status, created) = call(router, "CreateCluster", &create_cluster(name, "SMALL")).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateCluster must succeed, got {status} with {created}"
    );
    let cluster = Created {
        id: created["cluster"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        name: name.to_string(),
    };
    let (status, settled) = call(
        router,
        "GetCluster",
        &json!({"clusterIdentifier": &cluster.id}),
    )
    .await;
    assert_eq!(
        (status, settled["cluster"]["status"].as_str()),
        (200, Some("ACTIVE")),
        "case {case}: expected the cluster settled before the delete, got {status} with \
         {settled}"
    );
    cluster
}

/// Create a compute node group in `cluster` and settle it the same way.
async fn active_group(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    name: &str,
) -> Created {
    let (status, created) = call(
        router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(cluster, name),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: CreateComputeNodeGroup must succeed, got {status} with {created}"
    );
    let group = Created {
        id: created["computeNodeGroup"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        name: name.to_string(),
    };
    let (status, settled) = call(
        router,
        "GetComputeNodeGroup",
        &json!({
            "clusterIdentifier": cluster,
            "computeNodeGroupIdentifier": &group.id,
        }),
    )
    .await;
    assert_eq!(
        (status, settled["computeNodeGroup"]["status"].as_str()),
        (200, Some("ACTIVE")),
        "case {case}: expected the group settled before the delete, got {status} with \
         {settled}"
    );
    group
}

/// An accepted delete answers 200 with the operation's EMPTY output, which is what a
/// replay has to answer as well.
fn assert_empty_success(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, body),
        (200, &json!({})),
        "case {case}: expected the empty success the delete answers, got {status} with \
         {body}"
    );
}

#[tokio::test]
async fn a_retry_with_the_same_client_token_replays_the_cluster_delete() {
    // The rows: how the cluster is addressed, each at one of the two published token
    // lengths.
    let rows = [
        (Address::Id, shortest_token()),
        (Address::Id, longest_token()),
        (Address::Name, shortest_token()),
        (Address::Name, longest_token()),
    ];
    for (cluster_address, token) in rows {
        let case = format!(
            "cluster by {cluster_address:?}, token of {} characters",
            token.len()
        );
        let router = emulator();
        let cluster = active_cluster(&router, &case, "retry-cluster").await;
        let body = delete_cluster(cluster.as_addressed(cluster_address), Some(&token));

        let (status, first) = call(&router, "DeleteCluster", &body).await;
        assert_empty_success(&case, status, &first);

        // The removal the accepted delete owed is materialised before the next request is
        // answered, so a retry can only be answered from the remembered token.
        let (status, gone) = call(
            &router,
            "GetCluster",
            &json!({"clusterIdentifier": cluster.as_addressed(cluster_address)}),
        )
        .await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the cluster to be gone after its delete, got {status} \
             with {gone}"
        );

        // Byte-identical retries: each returns the first call's result, however many
        // times it is repeated.
        for attempt in 1..=2 {
            let (status, replayed) = call(&router, "DeleteCluster", &body).await;
            assert_empty_success(&format!("{case}, retry {attempt}"), status, &replayed);
        }

        // And the retry had no additional effect: the service holds no cluster at all.
        let (status, listed) = call(&router, "ListClusters", &json!({})).await;
        assert_eq!(
            (status, listed["clusters"].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {case}: expected the replayed delete to leave no cluster behind, got \
             {status} with {listed}"
        );
    }
}

#[tokio::test]
async fn a_cluster_delete_without_that_token_reports_the_cluster_gone() {
    // The rows: what the SECOND delete presents. None of them is a retry of the first
    // call — the SDK generates a fresh token per call when the caller supplies none — so
    // each is answered on the state it finds, which no longer holds the cluster.
    let rows = [
        ("no token at all", None),
        ("another token", Some("tok-0002".to_string())),
        ("a token of the maximum length", Some(longest_token())),
    ];
    for (case, second_token) in rows {
        let router = emulator();
        let cluster = active_cluster(&router, case, "fresh-cluster").await;

        let (status, first) = call(
            &router,
            "DeleteCluster",
            &delete_cluster(&cluster.id, Some(&shortest_token())),
        )
        .await;
        assert_empty_success(case, status, &first);

        let (status, again) = call(
            &router,
            "DeleteCluster",
            &delete_cluster(&cluster.id, second_token.as_deref()),
        )
        .await;
        assert_eq!(
            (status, error_code(&again).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected a delete presenting no retry of the first call to \
             report the cluster gone, got {status} with {again}"
        );
    }
}

#[tokio::test]
async fn a_cluster_delete_token_naming_another_cluster_conflicts() {
    // The rows: how the reused token addresses the OTHER cluster, which it must not reach
    // in either form.
    for other_address in [Address::Id, Address::Name] {
        let case = format!("the same token on another cluster, by {other_address:?}");
        let router = emulator();
        let cluster = active_cluster(&router, &case, "token-cluster").await;
        let other = active_cluster(&router, &case, "token-cluster-two").await;

        let token = shortest_token();
        let (status, first) = call(
            &router,
            "DeleteCluster",
            &delete_cluster(&cluster.id, Some(&token)),
        )
        .await;
        assert_empty_success(&case, status, &first);

        let (status, refused) = call(
            &router,
            "DeleteCluster",
            &delete_cluster(other.as_addressed(other_address), Some(&token)),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ConflictException"),
            "case {case}: expected a conflict, got {status} with {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(cluster.id.as_str()), Some("cluster")),
            "case {case}: expected the refusal to name the cluster the token was used \
             for, got {refused}"
        );

        // The cluster the reused token named is untouched: a conflict deletes nothing.
        let (status, read) = call(
            &router,
            "GetCluster",
            &json!({"clusterIdentifier": other.as_addressed(other_address)}),
        )
        .await;
        assert_eq!(
            (status, read["cluster"]["status"].as_str()),
            (200, Some("ACTIVE")),
            "case {case}: expected the refused cluster to stay as it was, got {status} \
             with {read}"
        );
    }
}

#[tokio::test]
async fn a_replayed_cluster_delete_leaves_a_recreated_cluster_alone() {
    // A retry "has no additional effect": once the name is free again and a NEW cluster
    // holds it, the retry still answers the first call's result and deletes nothing.
    let case = "a cluster created again under the deleted cluster's name";
    let router = emulator();
    let first_cluster = active_cluster(&router, case, "recreated-cluster").await;

    let token = longest_token();
    let (status, deleted) = call(
        &router,
        "DeleteCluster",
        &delete_cluster(&first_cluster.name, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &deleted);

    let second_cluster = active_cluster(&router, case, &first_cluster.name).await;
    assert_ne!(
        second_cluster.id, first_cluster.id,
        "case {case}: the cluster created again must carry an id of its own, got \
         {second_cluster:?}"
    );

    let (status, replayed) = call(
        &router,
        "DeleteCluster",
        &delete_cluster(&first_cluster.name, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &replayed);

    let (status, read) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": second_cluster.name}),
    )
    .await;
    assert_eq!(
        (
            status,
            read["cluster"]["id"].as_str(),
            read["cluster"]["status"].as_str()
        ),
        (200, Some(second_cluster.id.as_str()), Some("ACTIVE")),
        "case {case}: expected the replay to leave the cluster created again alone, got \
         {status} with {read}"
    );
}

#[tokio::test]
async fn a_retry_with_the_same_client_token_replays_the_group_delete() {
    // The rows: how the cluster and the group are addressed, each at one of the two
    // published token lengths.
    let rows = [
        (Address::Id, Address::Id, shortest_token()),
        (Address::Id, Address::Name, longest_token()),
        (Address::Name, Address::Id, longest_token()),
        (Address::Name, Address::Name, shortest_token()),
    ];
    for (cluster_address, group_address, token) in rows {
        let case = format!(
            "cluster by {cluster_address:?}, group by {group_address:?}, token of {} \
             characters",
            token.len()
        );
        let router = emulator();
        let cluster = active_cluster(&router, &case, "group-retry-cluster").await;
        let group = active_group(&router, &case, &cluster.name, "retry-group").await;
        let body = delete_group(
            cluster.as_addressed(cluster_address),
            group.as_addressed(group_address),
            Some(&token),
        );

        let (status, first) = call(&router, "DeleteComputeNodeGroup", &body).await;
        assert_empty_success(&case, status, &first);

        // A group this service is taking out is reported DELETED for one request before the
        // record is forgotten, so this retry lands while the record is still there and the
        // next one lands once it is gone: both answer the first call's result.
        let (status, replayed) = call(&router, "DeleteComputeNodeGroup", &body).await;
        let still_reported = format!("{case}, retry while still reported");
        assert_empty_success(&still_reported, status, &replayed);

        let (status, gone) = call(
            &router,
            "GetComputeNodeGroup",
            &json!({
                "clusterIdentifier": cluster.as_addressed(cluster_address),
                "computeNodeGroupIdentifier": group.as_addressed(group_address),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the group to be gone after its delete, got {status} \
             with {gone}"
        );

        let (status, replayed) = call(&router, "DeleteComputeNodeGroup", &body).await;
        assert_empty_success(&format!("{case}, retry once gone"), status, &replayed);

        // And the retries had no additional effect: the cluster holds no group, and it is
        // still there itself.
        let listing = json!({"clusterIdentifier": cluster.as_addressed(cluster_address)});
        let (status, listed) = call(&router, "ListComputeNodeGroups", &listing).await;
        assert_eq!(
            (
                status,
                listed["computeNodeGroups"].as_array().map(Vec::len)
            ),
            (200, Some(0)),
            "case {case}: expected the replayed delete to leave no group behind, got \
             {status} with {listed}"
        );
        let (status, read_cluster) = call(&router, "GetCluster", &listing).await;
        assert_eq!(
            (status, read_cluster["cluster"]["status"].as_str()),
            (200, Some("ACTIVE")),
            "case {case}: expected the cluster to be untouched by the replay, got \
             {status} with {read_cluster}"
        );
    }
}

#[tokio::test]
async fn a_group_delete_without_that_token_is_answered_on_the_state_it_finds() {
    // The rows: how far the service has carried the delete it owes by the time a second
    // delete that is NO retry arrives — one landing while the group is still reported on
    // its way out, and one landing after the record is gone — paired with what that second
    // call presents.
    let rows = [
        ("no token at all, group still reported", None, 0, "ConflictException"),
        (
            "another token, group still reported",
            Some("tok-0002".to_string()),
            0,
            "ConflictException",
        ),
        ("no token at all, group gone", None, 1, "ResourceNotFoundException"),
        (
            "a token of the maximum length, group gone",
            Some(longest_token()),
            1,
            "ResourceNotFoundException",
        ),
    ];
    for (case, second_token, intervening_reads, expected) in rows {
        let router = emulator();
        let cluster = active_cluster(&router, case, "group-fresh-cluster").await;
        let group = active_group(&router, case, &cluster.name, "fresh-group").await;

        let (status, first) = call(
            &router,
            "DeleteComputeNodeGroup",
            &delete_group(&cluster.id, &group.id, Some(&shortest_token())),
        )
        .await;
        assert_empty_success(case, status, &first);

        // Each request carries the delete one step further: the record is reported DELETED
        // by the first and forgotten by the second.
        for _ in 0..intervening_reads {
            let (status, _) = call(
                &router,
                "GetCluster",
                &json!({"clusterIdentifier": &cluster.id}),
            )
            .await;
            assert_eq!(status, 200, "case {case}: the cluster must still read back");
        }

        let (status, again) = call(
            &router,
            "DeleteComputeNodeGroup",
            &delete_group(&cluster.id, &group.id, second_token.as_deref()),
        )
        .await;
        assert_eq!(
            (status, error_code(&again).as_str()),
            (CLIENT_ERROR_STATUS, expected),
            "case {case}: expected a delete presenting no retry of the first call to be \
             answered {expected}, got {status} with {again}"
        );
    }
}

#[tokio::test]
async fn a_group_delete_token_naming_another_group_conflicts() {
    // The rows: the OTHER group the reused token names — one beside the deleted group in
    // the same cluster, and one of the same NAME in another cluster, which the token must
    // not reach either.
    for other_cluster in [false, true] {
        let case = if other_cluster {
            "the same token on a group of another cluster"
        } else {
            "the same token on another group of the same cluster"
        };
        let router = emulator();
        let cluster = active_cluster(&router, case, "group-token-cluster").await;
        let group = active_group(&router, case, &cluster.name, "token-group").await;
        let (other_cluster_ref, other_group) = if other_cluster {
            let second = active_cluster(&router, case, "group-token-cluster-two").await;
            let group = active_group(&router, case, &second.name, "token-group").await;
            (second, group)
        } else {
            let group = active_group(&router, case, &cluster.name, "token-group-two").await;
            (cluster.clone(), group)
        };

        let token = shortest_token();
        let (status, first) = call(
            &router,
            "DeleteComputeNodeGroup",
            &delete_group(&cluster.id, &group.id, Some(&token)),
        )
        .await;
        assert_empty_success(case, status, &first);

        let (status, refused) = call(
            &router,
            "DeleteComputeNodeGroup",
            &delete_group(&other_cluster_ref.id, &other_group.id, Some(&token)),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ConflictException"),
            "case {case}: expected a conflict, got {status} with {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(group.id.as_str()), Some("computenodegroup")),
            "case {case}: expected the refusal to name the group the token was used for, \
             got {refused}"
        );

        // The group the reused token named is untouched: a conflict deletes nothing.
        let (status, read) = call(
            &router,
            "GetComputeNodeGroup",
            &json!({
                "clusterIdentifier": other_cluster_ref.id,
                "computeNodeGroupIdentifier": other_group.id,
            }),
        )
        .await;
        assert_eq!(
            (status, read["computeNodeGroup"]["status"].as_str()),
            (200, Some("ACTIVE")),
            "case {case}: expected the refused group to stay as it was, got {status} with \
             {read}"
        );
    }
}

#[tokio::test]
async fn a_token_another_operation_used_is_a_cluster_delete_of_its_own() {
    // A token is remembered per OPERATION, so the same string on `CreateCluster` and then
    // on `DeleteCluster` is two requests, not a retry: the delete is carried out.
    let case = "the token CreateCluster used, presented to DeleteCluster";
    let router = emulator();

    let token = shortest_token();
    let mut create = create_cluster("shared-token-cluster", "SMALL");
    create["clientToken"] = json!(token);
    let (status, created) = call(&router, "CreateCluster", &create).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateCluster must succeed, got {status} with {created}"
    );
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (status, settled) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": &cluster_id}),
    )
    .await;
    assert_eq!(
        (status, settled["cluster"]["status"].as_str()),
        (200, Some("ACTIVE")),
        "case {case}: expected the cluster settled, got {status} with {settled}"
    );

    let (status, deleted) = call(
        &router,
        "DeleteCluster",
        &delete_cluster(&cluster_id, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &deleted);

    let (status, gone) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": &cluster_id}),
    )
    .await;
    assert_eq!(
        (status, error_code(&gone).as_str()),
        (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
        "case {case}: expected the delete to have been carried out rather than replayed \
         as the create, got {status} with {gone}"
    );
}
