// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `@idempotencyToken` of `DeleteQueue`, driven over the
//! awsJson1_0 wire through the same router the emulator serves from, on the harness
//! [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: `DeleteQueue` is `@idempotent` and its `clientToken` is an
//! `@idempotencyToken`, so "if the original request completes successfully, the subsequent
//! retries with the same client token return the result from the original successful
//! request and they have no additional effect"
//! (`smithy_query(query="doc",shape="DeleteQueueRequest",member="clientToken")`). A retry
//! presenting the token an ACCEPTED delete used is therefore answered with that call's
//! empty success, whichever form it addresses the cluster and the queue by, and changes
//! nothing — while a request that presents no token, or another token, is a delete of its
//! own and is answered on the state it finds, and a token an accepted delete used for
//! ANOTHER queue conflicts rather than silently skipping that queue's delete.
//!
//! The suites are tables. Their rows differ along what the rule generalises over: the form
//! each call addresses the cluster and the queue by (the generated id or the name, the two
//! forms the `*Identifier` patterns allow), and the length of the token itself at both of
//! `SBClientToken`'s published bounds.
//!
//! Nothing here waits on the wall clock: a transition this service owes is materialised
//! before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{call, create_cluster, create_queue, emulator};

/// How a call addresses a resource: by the id PCS generated for it, or by the name its
/// create chose — the two forms `ClusterIdentifier` and `QueueIdentifier` both allow
/// (`smithy_query(query="shape",shape="QueueIdentifier")`:
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

/// A `DeleteQueue` request for `queue` of `cluster`, carrying `token` when the case
/// presents one.
fn delete_queue(cluster: &str, queue: &str, token: Option<&str>) -> Value {
    let mut body = json!({"clusterIdentifier": cluster, "queueIdentifier": queue});
    if let Some(token) = token {
        body["clientToken"] = json!(token);
    }
    body
}

/// Create a cluster and hand back what it was created as. The request that follows a
/// create observes the resource settled, so no case waits on a clock.
async fn active_cluster(router: &MultiServiceRouter, case: &str, name: &str) -> Created {
    let (status, created) = call(router, "CreateCluster", &create_cluster(name, "SMALL")).await;
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

/// Create a queue in `cluster`, named the way that row addresses it.
async fn active_queue(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    name: &str,
) -> Created {
    let (status, created) = call(router, "CreateQueue", &create_queue(cluster, name, &[])).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateQueue must succeed, got {status} with {created}"
    );
    Created {
        id: created["queue"]["id"].as_str().unwrap_or_default().to_string(),
        name: name.to_string(),
    }
}

/// An accepted `DeleteQueue` answers 200 with the operation's EMPTY output, which is what
/// a replay has to answer as well.
fn assert_empty_success(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, body),
        (200, &json!({})),
        "case {case}: expected the empty success DeleteQueue answers, got {status} with \
         {body}"
    );
}

#[tokio::test]
async fn a_retry_with_the_same_client_token_replays_the_delete() {
    // The rows: every combination of how the cluster and the queue are addressed, each at
    // one of the two published token lengths.
    let rows = [
        (Address::Id, Address::Id, shortest_token()),
        (Address::Id, Address::Name, longest_token()),
        (Address::Name, Address::Id, longest_token()),
        (Address::Name, Address::Name, shortest_token()),
    ];
    for (cluster_address, queue_address, token) in rows {
        let case = format!(
            "cluster by {cluster_address:?}, queue by {queue_address:?}, token of {} \
             characters",
            token.len()
        );
        let router = emulator();
        let cluster = active_cluster(&router, &case, "retry-cluster").await;
        let queue = active_queue(&router, &case, &cluster.name, "retry-queue").await;
        let body = delete_queue(
            cluster.as_addressed(cluster_address),
            queue.as_addressed(queue_address),
            Some(&token),
        );

        let (status, first) = call(&router, "DeleteQueue", &body).await;
        assert_empty_success(&case, status, &first);

        // The queue the accepted delete owed is gone by the next request, so a retry can
        // only be answered from the remembered token.
        let read = json!({
            "clusterIdentifier": cluster.as_addressed(cluster_address),
            "queueIdentifier": queue.as_addressed(queue_address),
        });
        let (status, gone) = call(&router, "GetQueue", &read).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the queue to be gone after its delete, got {status} \
             with {gone}"
        );

        // Byte-identical retries: each returns the first call's result, however many
        // times it is repeated.
        for attempt in 1..=2 {
            let (status, replayed) = call(&router, "DeleteQueue", &body).await;
            assert_empty_success(&format!("{case}, retry {attempt}"), status, &replayed);
        }

        // And the retry had no additional effect: the cluster still holds no queue, and it
        // is still there itself.
        let listing = json!({"clusterIdentifier": cluster.as_addressed(cluster_address)});
        let (status, listed) = call(&router, "ListQueues", &listing).await;
        assert_eq!(
            (status, listed["queues"].as_array().map(Vec::len)),
            (200, Some(0)),
            "case {case}: expected the replayed delete to leave no queue behind, got \
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
async fn a_delete_without_that_token_reports_the_queue_gone() {
    // The rows: what the SECOND delete presents. None of them is a retry of the first
    // call — the SDK generates a fresh token per call when the caller supplies none — so
    // each is answered on the state it finds, which no longer holds the queue.
    let rows = [
        ("no token at all", None),
        ("another token", Some("tok-0002".to_string())),
        ("a token of the maximum length", Some(longest_token())),
    ];
    for (case, second_token) in rows {
        let router = emulator();
        let cluster = active_cluster(&router, case, "fresh-cluster").await;
        let queue = active_queue(&router, case, &cluster.name, "fresh-queue").await;

        let (status, first) = call(
            &router,
            "DeleteQueue",
            &delete_queue(&cluster.id, &queue.id, Some(&shortest_token())),
        )
        .await;
        assert_empty_success(case, status, &first);

        let (status, again) = call(
            &router,
            "DeleteQueue",
            &delete_queue(&cluster.id, &queue.id, second_token.as_deref()),
        )
        .await;
        assert_eq!(
            (status, error_code(&again).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected a delete presenting no retry of the first call to \
             report the queue gone, got {status} with {again}"
        );
    }
}

#[tokio::test]
async fn a_delete_token_naming_another_queue_conflicts() {
    // The rows: the OTHER queue the reused token names — one beside the deleted queue in
    // the same cluster, and one of the same NAME in another cluster, which the token must
    // not reach either.
    for other_cluster in [false, true] {
        let case = if other_cluster {
            "the same token on a queue of another cluster"
        } else {
            "the same token on another queue of the same cluster"
        };
        let router = emulator();
        let cluster = active_cluster(&router, case, "token-cluster").await;
        let queue = active_queue(&router, case, &cluster.name, "token-queue").await;
        let (other_cluster_ref, other_queue) = if other_cluster {
            let second = active_cluster(&router, case, "token-cluster-two").await;
            let queue = active_queue(&router, case, &second.name, "token-queue").await;
            (second, queue)
        } else {
            let queue = active_queue(&router, case, &cluster.name, "token-queue-two").await;
            (cluster.clone(), queue)
        };

        let token = shortest_token();
        let (status, first) = call(
            &router,
            "DeleteQueue",
            &delete_queue(&cluster.id, &queue.id, Some(&token)),
        )
        .await;
        assert_empty_success(case, status, &first);

        let (status, refused) = call(
            &router,
            "DeleteQueue",
            &delete_queue(&other_cluster_ref.id, &other_queue.id, Some(&token)),
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
            (Some(queue.id.as_str()), Some("queue")),
            "case {case}: expected the refusal to name the queue the token was used for, \
             got {refused}"
        );

        // The queue the reused token named is untouched: a conflict deletes nothing.
        let (status, read) = call(
            &router,
            "GetQueue",
            &json!({
                "clusterIdentifier": other_cluster_ref.id,
                "queueIdentifier": other_queue.id,
            }),
        )
        .await;
        assert_eq!(
            (status, read["queue"]["status"].as_str()),
            (200, Some("ACTIVE")),
            "case {case}: expected the refused queue to stay as it was, got {status} with \
             {read}"
        );
    }
}

#[tokio::test]
async fn a_replayed_delete_leaves_a_requeued_queue_alone() {
    // A retry "has no additional effect": once the name is free again and a NEW queue
    // holds it, the retry still answers the first call's result and deletes nothing.
    let case = "a queue created again under the deleted queue's name";
    let router = emulator();
    let cluster = active_cluster(&router, case, "requeue-cluster").await;
    let first_queue = active_queue(&router, case, &cluster.name, "requeue-queue").await;

    let token = shortest_token();
    let (status, deleted) = call(
        &router,
        "DeleteQueue",
        &delete_queue(&cluster.name, &first_queue.name, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &deleted);

    let second_queue = active_queue(&router, case, &cluster.name, &first_queue.name).await;
    assert_ne!(
        second_queue.id, first_queue.id,
        "case {case}: the queue created again must carry an id of its own, got \
         {second_queue:?}"
    );

    let (status, replayed) = call(
        &router,
        "DeleteQueue",
        &delete_queue(&cluster.name, &first_queue.name, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &replayed);

    let (status, read) = call(
        &router,
        "GetQueue",
        &json!({"clusterIdentifier": cluster.name, "queueIdentifier": second_queue.name}),
    )
    .await;
    assert_eq!(
        (
            status,
            read["queue"]["id"].as_str(),
            read["queue"]["status"].as_str()
        ),
        (200, Some(second_queue.id.as_str()), Some("ACTIVE")),
        "case {case}: expected the replay to leave the queue created again alone, got \
         {status} with {read}"
    );
}

#[tokio::test]
async fn a_token_another_operation_used_is_a_delete_of_its_own() {
    // A token is remembered per OPERATION, so the same string on `CreateQueue` and then on
    // `DeleteQueue` is two requests, not a retry: the delete is carried out.
    let case = "the token CreateQueue used, presented to DeleteQueue";
    let router = emulator();
    let cluster = active_cluster(&router, case, "shared-token-cluster").await;

    let token = shortest_token();
    let mut create = create_queue(&cluster.name, "shared-token-queue", &[]);
    create["clientToken"] = json!(token);
    let (status, created) = call(&router, "CreateQueue", &create).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateQueue must succeed, got {status} with {created}"
    );
    let queue_id = created["queue"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (status, deleted) = call(
        &router,
        "DeleteQueue",
        &delete_queue(&cluster.name, &queue_id, Some(&token)),
    )
    .await;
    assert_empty_success(case, status, &deleted);

    let (status, gone) = call(
        &router,
        "GetQueue",
        &json!({"clusterIdentifier": cluster.name, "queueIdentifier": queue_id}),
    )
    .await;
    assert_eq!(
        (status, error_code(&gone).as_str()),
        (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
        "case {case}: expected the delete to have been carried out rather than replayed \
         as the create, got {status} with {gone}"
    );
}
