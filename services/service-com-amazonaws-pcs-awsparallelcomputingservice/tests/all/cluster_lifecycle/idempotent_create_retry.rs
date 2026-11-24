// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `@idempotencyToken` of the PCS CREATE operations, driven over
//! the awsJson1_0 wire through the same router the emulator serves from, on the harness
//! [`crate::cluster_lifecycle`] builds.
//!
//! RULE under test: a `clientToken` replay answers "the result from the original
//! successful request" and has "no additional effect"
//! (`smithy_query(query="doc",shape="CreateQueueRequest",member="clientToken")`), so a
//! retry is the first call only when it asks for the SAME resource that call minted — the
//! same `@required` name, in the same parent. A request that reuses a token while naming
//! another resource is a different request: it is refused with the modelled
//! `ConflictException` naming the resource the token was used for, and neither the
//! remembered resource nor the one it asked for is reported as created.
//!
//! The suites are tables over the three creates that mint a named resource
//! (`CreateCluster`, `CreateComputeNodeGroup`, `CreateQueue`). Their rows differ along
//! what the rule generalises over: HOW the retry's name differs from the first call's
//! (another name, the same name in another case, one character more, a prefix) — names
//! are case-sensitive and compared whole — and, for the replaying suite, what the retry
//! changes BESIDES the name, which a replay has no additional effect for.
//!
//! Nothing here waits on the wall clock: a transition this service owes is materialised
//! before the next request is answered.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use super::{call, create_cluster, create_compute_node_group, create_queue, emulator};

/// A token of the length `SBClientToken` admits
/// (`smithy_query(query="shape",shape="SBClientToken")`: `@length(min: 8, max: 100)`).
const TOKEN: &str = "create-token-0001";

/// The compute node group every queue in these suites assigns jobs to.
const GROUP: &str = "nodes";

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// One row of the conflicting suite: what the difference is called, and how it renames
/// the first call's name.
type RetryName = (&'static str, fn(&str) -> String);

/// One row of the replaying suite: what the difference is called, and how it varies the
/// retry's request beside the name it asks for.
type RetryVariant = (&'static str, fn(&mut Value));

/// One create under test: the operation, the member its answer travels in, and how its
/// resource is read back.
#[derive(Debug, Clone, Copy)]
struct Create {
    /// The create operation's name.
    operation: &'static str,
    /// The output member the created resource travels in.
    member: &'static str,
    /// The noun the service's `clientToken` conflict names this kind of resource by.
    noun: &'static str,
    /// The `resourceType` that conflict reports.
    resource_type: &'static str,
    /// The read that answers with one such resource, and the member it addresses it by.
    reader: &'static str,
    /// The identifier member `reader` takes.
    reader_member: &'static str,
    /// The list that reports every such resource, and the member its page travels in.
    lister: &'static str,
    /// The output member `lister`'s page travels in.
    listed_member: &'static str,
    /// The name the FIRST call of each case creates its resource under.
    first_name: &'static str,
}

/// `CreateCluster`, whose resource has no parent.
const CLUSTER: Create = Create {
    operation: "CreateCluster",
    member: "cluster",
    noun: "cluster",
    resource_type: "cluster",
    reader: "GetCluster",
    reader_member: "clusterIdentifier",
    lister: "ListClusters",
    listed_member: "clusters",
    first_name: "token-cluster",
};

/// `CreateComputeNodeGroup`, scoped to its cluster.
const COMPUTE_NODE_GROUP: Create = Create {
    operation: "CreateComputeNodeGroup",
    member: "computeNodeGroup",
    noun: "compute node group",
    resource_type: "computenodegroup",
    reader: "GetComputeNodeGroup",
    reader_member: "computeNodeGroupIdentifier",
    lister: "ListComputeNodeGroups",
    listed_member: "computeNodeGroups",
    first_name: "token-nodes",
};

/// `CreateQueue`, scoped to its cluster.
const QUEUE: Create = Create {
    operation: "CreateQueue",
    member: "queue",
    noun: "queue",
    resource_type: "queue",
    reader: "GetQueue",
    reader_member: "queueIdentifier",
    lister: "ListQueues",
    listed_member: "queues",
    first_name: "token-work",
};

/// Every create under test.
const CREATES: [Create; 3] = [CLUSTER, COMPUTE_NODE_GROUP, QUEUE];

/// A fresh emulator holding, for the two child creates, the cluster they are scoped to
/// with one compute node group in it. `None` for [`CLUSTER`], whose resource has no
/// parent.
///
/// Every resource is settled by the request that follows its own create, so there is
/// nothing to wait for.
async fn harness(create: Create) -> (MultiServiceRouter, Option<String>) {
    let router = emulator();
    if create.operation == CLUSTER.operation {
        return (router, None);
    }
    let (status, body) = call(
        &router,
        "CreateCluster",
        &create_cluster("token-host", "SMALL"),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateCluster: got {status} {body}");
    let cluster = body["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, group) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group(&cluster, GROUP),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateComputeNodeGroup: got {status} {group}"
    );
    (router, Some(cluster))
}

/// The create request `create` takes for a resource called `name` in `cluster`.
fn request(create: Create, cluster: Option<&str>, name: &str) -> Value {
    let cluster = cluster.unwrap_or_default();
    match create.operation {
        "CreateCluster" => create_cluster(name, "SMALL"),
        "CreateComputeNodeGroup" => create_compute_node_group(cluster, name),
        _ => create_queue(cluster, name, &[GROUP]),
    }
}

/// The read `create.reader` takes for the resource `identifier` names.
fn read_request(create: Create, cluster: Option<&str>, identifier: &str) -> Value {
    let mut body = json!({create.reader_member: identifier});
    if let Some(cluster) = cluster {
        body["clusterIdentifier"] = json!(cluster);
    }
    body
}

/// The list `create.lister` takes.
fn list_request(cluster: Option<&str>) -> Value {
    match cluster {
        Some(cluster) => json!({"clusterIdentifier": cluster}),
        None => json!({}),
    }
}

/// The names the parent cluster of a child create holds of its own, which its list
/// reports beside the resources a case creates: the compute node group every queue
/// associates is one such entry in `ListComputeNodeGroups`.
fn pre_existing(create: Create) -> usize {
    usize::from(create.operation == COMPUTE_NODE_GROUP.operation)
}

#[tokio::test]
async fn a_create_token_replayed_under_another_name_conflicts_and_creates_nothing() {
    // Rows: how the retry's name differs from the first call's. Each is a name the
    // resource's own `@pattern` and `@length` admit
    // (`smithy_query(query="shape",shape="ClusterName,QueueName,ComputeNodeGroupName")`:
    // `^(?!pcs_)^[A-Za-z][A-Za-z0-9-]+$`, at least 3 characters), built from the first
    // call's name so every row differs ONLY in the name the retry asks for.
    let retry_names: [RetryName; 4] = [
        ("another name entirely", |_| "other-name".to_string()),
        ("the same name in another case", |first| first.to_uppercase()),
        ("one character more", |first| format!("{first}2")),
        ("a prefix of it", |first| first[..3].to_string()),
    ];

    for create in CREATES {
        for (difference, rename) in retry_names {
            let (router, cluster) = harness(create).await;
            let case = format!("{} retried with {difference}", create.operation);

            let mut first = request(create, cluster.as_deref(), create.first_name);
            first["clientToken"] = json!(TOKEN);
            let (status, created) = call(&router, create.operation, &first).await;
            assert_eq!(status, 200, "case {case}: first call: got {status} {created}");
            let first_id = created[create.member]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            let other_name = rename(create.first_name);
            let mut retry = request(create, cluster.as_deref(), &other_name);
            retry["clientToken"] = json!(TOKEN);
            let (status, refused) = call(&router, create.operation, &retry).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ConflictException"),
                "case {case}: expected the reused token to conflict, got {status} {refused}"
            );
            assert_eq!(
                refused["message"],
                json!(format!(
                    "clientToken was already used to create {} {first_id}",
                    create.noun
                )),
                "case {case}: expected the refusal to name the resource the token created, \
                 got {refused}"
            );
            assert_eq!(
                (
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (Some(first_id.as_str()), Some(create.resource_type)),
                "case {case}: expected the refusal to carry the first call's resource, got \
                 {refused}"
            );

            // Nothing was created for the name the retry asked for.
            let (status, missing) = call(
                &router,
                create.reader,
                &read_request(create, cluster.as_deref(), &other_name),
            )
            .await;
            assert_eq!(
                (status, error_code(&missing).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected {other_name} not to have been created, got {status} \
                 {missing}"
            );

            // And the first call's resource is still the only one of its kind.
            let (status, listed) = call(&router, create.lister, &list_request(cluster.as_deref()))
                .await;
            assert_eq!(status, 200, "case {case}: {} got {listed}", create.lister);
            let page = listed[create.listed_member]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                page.len(),
                1 + pre_existing(create),
                "case {case}: expected the retry to have created nothing, got {listed}"
            );
            assert!(
                page.iter().any(|entry| entry["id"] == json!(&first_id)),
                "case {case}: expected the first call's resource to still be listed, got \
                 {listed}"
            );
            assert!(
                !page
                    .iter()
                    .any(|entry| entry["name"] == json!(other_name.as_str())),
                "case {case}: expected no resource named {other_name}, got {listed}"
            );
        }
    }
}

#[tokio::test]
async fn a_create_token_replayed_under_the_same_name_replays_the_first_call() {
    // Rows: what the retry changes BESIDES the name it asks for. A retry that asks for
    // the same resource replays the first call's result and has no additional effect,
    // whatever else it carries.
    let variants: [RetryVariant; 3] = [
        ("the request verbatim", |_| {}),
        ("a tag the first call did not carry", |retry| {
            retry["tags"] = json!({"team": "hpc"});
        }),
        ("no compute node group configuration", |retry| {
            if retry.get("computeNodeGroupConfigurations").is_some() {
                retry["computeNodeGroupConfigurations"] = json!([]);
            }
        }),
    ];

    for create in CREATES {
        for (difference, vary) in variants {
            let (router, cluster) = harness(create).await;
            let case = format!("{} retried with {difference}", create.operation);

            let mut first = request(create, cluster.as_deref(), create.first_name);
            first["clientToken"] = json!(TOKEN);
            let (status, created) = call(&router, create.operation, &first).await;
            assert_eq!(status, 200, "case {case}: first call: got {status} {created}");
            let created = created[create.member].clone();

            let mut retry = request(create, cluster.as_deref(), create.first_name);
            retry["clientToken"] = json!(TOKEN);
            vary(&mut retry);
            let (status, replayed) = call(&router, create.operation, &retry).await;
            assert_eq!(status, 200, "case {case}: retry: got {status} {replayed}");
            let replayed = replayed[create.member].clone();
            assert_eq!(
                (
                    replayed["id"].as_str(),
                    replayed["name"].as_str(),
                    replayed["arn"].as_str()
                ),
                (
                    created["id"].as_str(),
                    created["name"].as_str(),
                    created["arn"].as_str()
                ),
                "case {case}: expected the first call's resource, got {replayed}"
            );

            let (status, listed) = call(&router, create.lister, &list_request(cluster.as_deref()))
                .await;
            assert_eq!(status, 200, "case {case}: {} got {listed}", create.lister);
            assert_eq!(
                listed[create.listed_member].as_array().map(Vec::len),
                Some(1 + pre_existing(create)),
                "case {case}: expected the replay to have created nothing, got {listed}"
            );
        }
    }
}

#[tokio::test]
async fn one_token_creates_one_resource_of_each_kind() {
    // A remembered token is scoped to the operation that used it, so the same token
    // naming a cluster, a compute node group and a queue creates all three: each create
    // is the first call of its own operation, not a retry of another's.
    let router = emulator();

    let mut cluster_request = create_cluster("token-shared", "SMALL");
    cluster_request["clientToken"] = json!(TOKEN);
    let (status, cluster) = call(&router, "CreateCluster", &cluster_request).await;
    assert_eq!(status, 200, "case CreateCluster: got {status} {cluster}");
    let cluster_id = cluster["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let mut group_request = create_compute_node_group(&cluster_id, GROUP);
    group_request["clientToken"] = json!(TOKEN);
    let (status, group) = call(&router, "CreateComputeNodeGroup", &group_request).await;
    assert_eq!(
        status, 200,
        "case CreateComputeNodeGroup on the same token: got {status} {group}"
    );

    let mut queue_request = create_queue(&cluster_id, "token-work", &[GROUP]);
    queue_request["clientToken"] = json!(TOKEN);
    let (status, queue) = call(&router, "CreateQueue", &queue_request).await;
    assert_eq!(
        status, 200,
        "case CreateQueue on the same token: got {status} {queue}"
    );
    assert_eq!(
        queue["queue"]["name"],
        json!("token-work"),
        "case CreateQueue on the same token: expected the queue the request named, got \
         {queue}"
    );
}
