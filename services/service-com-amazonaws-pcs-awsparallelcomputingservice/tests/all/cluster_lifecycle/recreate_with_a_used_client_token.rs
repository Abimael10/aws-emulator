// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a CREATE that presents an `@idempotencyToken` a previous create
//! used for a resource this service no longer holds, driven over the awsJson1_0 wire
//! through the same router the emulator serves from, on the harness
//! [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a `clientToken` retry "return[s] the result from the original
//! successful request" and has "no additional effect"
//! (`smithy_query(query="doc",shape="CreateQueueRequest",member="clientToken")`, the same
//! sentence on `CreateClusterRequest$clientToken` and
//! `CreateComputeNodeGroupRequest$clientToken`). That result is the resource the first
//! call minted, so a token is only answerable for as long as that resource is stored:
//! once it has been deleted there is no original result to replay and nothing for a reused
//! token to be held against, so the request is a create of its own — it is answered on the
//! state the service holds, and the token then names what it creates.
//!
//! The suites are tables over the three creates that mint a named resource
//! (`CreateCluster`, `CreateComputeNodeGroup`, `CreateQueue`), so the rule is checked
//! wherever it holds and not only on the operation it was reported on. Their rows differ
//! along what the rule generalises over: WHICH create the token was used by, and whether
//! the resource the token was used for is still stored when the token comes back.
//!
//! Nothing here waits on the wall clock: a removal this service owes is materialised by
//! the requests that follow the delete, which is what the poll below looks for.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use super::{call, create_cluster, create_compute_node_group, create_queue, emulator};

/// A token of a length `SBClientToken` admits
/// (`smithy_query(query="shape",shape="SBClientToken")`: `@length(min: 8, max: 100)`).
const TOKEN: &str = "recreate-token-0001";

/// How many requests a case gives this service to materialise a removal it owes. A record
/// on its way out is taken through at most its `DELETING`/`DELETED` statuses and then
/// dropped, so two requests are enough; the bound is generous so a case cannot fail for
/// being one request short.
const SETTLING_REQUESTS: usize = 16;

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// One create under test: the operation, how its resource is read back, deleted and
/// listed, and the names its cases create it under.
#[derive(Debug, Clone, Copy)]
struct Create {
    /// The create operation's name.
    operation: &'static str,
    /// The output member the created resource travels in.
    member: &'static str,
    /// The read that answers with one such resource.
    reader: &'static str,
    /// The delete that takes one out.
    deleter: &'static str,
    /// The member `reader` and `deleter` address the resource by.
    identifier_member: &'static str,
    /// The list that reports every such resource.
    lister: &'static str,
    /// The output member `lister`'s page travels in.
    listed_member: &'static str,
    /// Whether the resource lives inside a cluster, so every call naming it carries a
    /// `clusterIdentifier` as well.
    inside_a_cluster: bool,
    /// The name the first call of each case creates its resource under.
    first_name: &'static str,
    /// A second name, for the case whose later call asks for another resource.
    other_name: &'static str,
}

/// `CreateCluster`, whose resource has no parent.
const CLUSTER: Create = Create {
    operation: "CreateCluster",
    member: "cluster",
    reader: "GetCluster",
    deleter: "DeleteCluster",
    identifier_member: "clusterIdentifier",
    lister: "ListClusters",
    listed_member: "clusters",
    inside_a_cluster: false,
    first_name: "reuse-cluster",
    other_name: "reuse-cluster-two",
};

/// `CreateComputeNodeGroup`, scoped to its cluster.
const COMPUTE_NODE_GROUP: Create = Create {
    operation: "CreateComputeNodeGroup",
    member: "computeNodeGroup",
    reader: "GetComputeNodeGroup",
    deleter: "DeleteComputeNodeGroup",
    identifier_member: "computeNodeGroupIdentifier",
    lister: "ListComputeNodeGroups",
    listed_member: "computeNodeGroups",
    inside_a_cluster: true,
    first_name: "reuse-nodes",
    other_name: "reuse-nodes-two",
};

/// `CreateQueue`, scoped to its cluster.
const QUEUE: Create = Create {
    operation: "CreateQueue",
    member: "queue",
    reader: "GetQueue",
    deleter: "DeleteQueue",
    identifier_member: "queueIdentifier",
    lister: "ListQueues",
    listed_member: "queues",
    inside_a_cluster: true,
    first_name: "reuse-work",
    other_name: "reuse-work-two",
};

/// Every create under test.
const CREATES: [Create; 3] = [CLUSTER, COMPUTE_NODE_GROUP, QUEUE];

/// A fresh emulator holding, for the two child creates, the cluster they are scoped to.
/// `None` for [`CLUSTER`], whose resource has no parent.
async fn harness(create: Create, case: &str) -> (MultiServiceRouter, Option<String>) {
    let router = emulator();
    if !create.inside_a_cluster {
        return (router, None);
    }
    let (status, body) = call(
        &router,
        "CreateCluster",
        &create_cluster("reuse-host", "SMALL"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: setup CreateCluster: got {status} {body}"
    );
    let cluster = body["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (router, Some(cluster))
}

/// The create request `create` takes for a resource called `name` in `cluster`, carrying
/// `token`.
fn create_request(create: Create, cluster: Option<&str>, name: &str, token: &str) -> Value {
    let parent = cluster.unwrap_or_default();
    let mut body = match create.operation {
        "CreateCluster" => create_cluster(name, "SMALL"),
        "CreateComputeNodeGroup" => create_compute_node_group(parent, name),
        _ => create_queue(parent, name, &[]),
    };
    body["clientToken"] = json!(token);
    body
}

/// The body a read or a delete of the resource `identifier` names takes. Both address a
/// resource by its GENERATED id here, which is the form that resolves to nothing once the
/// record is gone.
fn addressed(create: Create, cluster: Option<&str>, identifier: &str) -> Value {
    let mut body = json!({create.identifier_member: identifier});
    if let Some(cluster) = cluster {
        body["clusterIdentifier"] = json!(cluster);
    }
    body
}

/// The body `create.lister` takes.
fn list_request(cluster: Option<&str>) -> Value {
    match cluster {
        Some(cluster) => json!({"clusterIdentifier": cluster}),
        None => json!({}),
    }
}

/// Create the resource `name` with `token` and hand back its generated id.
async fn create_with_token(
    router: &MultiServiceRouter,
    create: Create,
    cluster: Option<&str>,
    name: &str,
    token: &str,
    case: &str,
) -> String {
    let (status, created) = call(
        router,
        create.operation,
        &create_request(create, cluster, name, token),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: {} for {name} must succeed, got {status} {created}",
        create.operation
    );
    created[create.member]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Delete the resource `identifier` names and keep reading it back until this service no
/// longer holds it.
///
/// Each request is what materialises the removal already owed, so this waits for the
/// CONDITION — the read no longer resolving the record — rather than for any amount of
/// time.
async fn delete_and_await_removal(
    router: &MultiServiceRouter,
    create: Create,
    cluster: Option<&str>,
    identifier: &str,
    case: &str,
) {
    let body = addressed(create, cluster, identifier);
    let (status, deleted) = call(router, create.deleter, &body).await;
    assert_eq!(
        status, 200,
        "case {case}: {} must be accepted, got {status} {deleted}",
        create.deleter
    );
    for _ in 0..SETTLING_REQUESTS {
        let (status, read) = call(router, create.reader, &body).await;
        if (status, error_code(&read).as_str()) == (CLIENT_ERROR_STATUS, "ResourceNotFoundException")
        {
            return;
        }
        assert_eq!(
            status, 200,
            "case {case}: {} while the record is settling: got {status} {read}",
            create.reader
        );
    }
    panic!(
        "case {case}: expected {} to stop resolving {identifier} within {SETTLING_REQUESTS} \
         requests",
        create.reader
    );
}

#[tokio::test]
async fn a_create_token_whose_resource_was_deleted_creates_again() {
    for create in CREATES {
        let case = format!("{} after its resource was deleted", create.operation);
        let (router, cluster) = harness(create, &case).await;

        let first_id = create_with_token(
            &router,
            create,
            cluster.as_deref(),
            create.first_name,
            TOKEN,
            &case,
        )
        .await;
        delete_and_await_removal(&router, create, cluster.as_deref(), &first_id, &case).await;

        // The token belongs to a create the caller is making again — a deterministic token
        // derived from the resource's own name does exactly this — and the resource it
        // named is gone, so there is nothing to replay and nothing to refuse.
        let (status, recreated) = call(
            &router,
            create.operation,
            &create_request(create, cluster.as_deref(), create.first_name, TOKEN),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the create to be answered on the state the service \
             holds, got {status} {recreated}"
        );
        let second = recreated[create.member].clone();
        let second_id = second["id"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            second["name"],
            json!(create.first_name),
            "case {case}: expected the resource the request named, got {second}"
        );
        assert_ne!(
            second_id, first_id,
            "case {case}: expected a newly minted resource rather than the deleted one's \
             id, got {second}"
        );

        // Round trip: the recreated resource is readable, by the id the create answered.
        let (status, read) = call(
            &router,
            create.reader,
            &addressed(create, cluster.as_deref(), &second_id),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: {} must answer with the recreated resource, got {status} {read}",
            create.reader
        );
        assert_eq!(
            (
                read[create.member]["id"].as_str(),
                read[create.member]["name"].as_str()
            ),
            (Some(second_id.as_str()), Some(create.first_name)),
            "case {case}: expected the read to answer the recreated resource, got {read}"
        );

        // And the token now names what it created: a retry of THAT call replays the new
        // resource and creates nothing further.
        let (status, replayed) = call(
            &router,
            create.operation,
            &create_request(create, cluster.as_deref(), create.first_name, TOKEN),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the retry to replay the recreated resource, got {status} \
             {replayed}"
        );
        assert_eq!(
            replayed[create.member]["id"].as_str(),
            Some(second_id.as_str()),
            "case {case}: expected the retry to answer the resource the token now names, \
             got {replayed}"
        );

        let (status, listed) = call(&router, create.lister, &list_request(cluster.as_deref())).await;
        assert_eq!(
            status, 200,
            "case {case}: {} got {status} {listed}",
            create.lister
        );
        let page = listed[create.listed_member]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            page.len(),
            1,
            "case {case}: expected exactly the recreated resource to be stored, got {listed}"
        );
        assert_eq!(
            page.first().and_then(|entry| entry["id"].as_str()),
            Some(second_id.as_str()),
            "case {case}: expected the stored resource to be the recreated one, got {listed}"
        );
    }
}

#[tokio::test]
async fn a_create_token_is_held_against_a_request_only_while_its_resource_is_stored() {
    // Rows: whether the resource the token was used for is still stored when the token
    // comes back under ANOTHER name. While it is, the request is not that call's retry and
    // is refused; once it is gone the token is answerable for nothing, so the request is a
    // create of its own.
    for create in CREATES {
        let case = format!("{} under another name", create.operation);
        let (router, cluster) = harness(create, &case).await;

        let first_id = create_with_token(
            &router,
            create,
            cluster.as_deref(),
            create.first_name,
            TOKEN,
            &case,
        )
        .await;

        // Still stored: the token names a resource this request is not asking for.
        let (status, refused) = call(
            &router,
            create.operation,
            &create_request(create, cluster.as_deref(), create.other_name, TOKEN),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ConflictException"),
            "case {case}: expected the reused token to conflict while its resource is \
             stored, got {status} {refused}"
        );
        assert_eq!(
            refused["resourceId"].as_str(),
            Some(first_id.as_str()),
            "case {case}: expected the refusal to name the resource the token was used \
             for, got {refused}"
        );

        delete_and_await_removal(&router, create, cluster.as_deref(), &first_id, &case).await;

        // Gone: the same request is now answered on the state the service holds.
        let (status, created) = call(
            &router,
            create.operation,
            &create_request(create, cluster.as_deref(), create.other_name, TOKEN),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the create to be answered once the token's resource is \
             gone, got {status} {created}"
        );
        assert_eq!(
            created[create.member]["name"].as_str(),
            Some(create.other_name),
            "case {case}: expected the resource the request named, got {created}"
        );
    }
}
