// SPDX-License-Identifier: CC0-1.0

//! The cluster surface a client reaches once a cluster exists: its TAGS, its NODES, and
//! the subnet group it is placed in.
//!
//! `TagResource`/`ListTags`/`UntagResource` address a cluster by ARN and keep one tag per
//! key; `RebootNode` and `DecreaseReplicationFactor` address its nodes by the id the
//! cluster minted; `DeleteSubnetGroup` removes a group no cluster is placed in. Each case
//! drives `POST /` with `X-Amz-Target: AmazonDAXV3.<Operation>` through the same
//! `MultiServiceRouter` the production router builds, so what is asserted is the HTTP
//! behaviour a DAX SDK client sees, and every assertion is read back out of the store
//! through a describe or a list rather than from the answering call alone.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::dax_account_network::{
    account_subnet_ids, call, cluster_in_status, emulator, error_code, error_message,
    LOOK_AGAIN_AFTER, REGION, SETTLE_DEADLINE,
};

/// The canonical emulator account id every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A role ARN to pass as the required `IamRoleArn`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DAXRole";

/// A node type to pass as the required `NodeType`.
const NODE_TYPE: &str = "dax.t3.small";

/// The status a client fault answers with on this service's JSON protocol.
const CLIENT_FAULT_STATUS: u16 = 400;

/// A `CreateCluster` body with the required members filled in.
fn create_cluster_body(name: &str, replication_factor: i64) -> Value {
    json!({
        "ClusterName": name,
        "NodeType": NODE_TYPE,
        "ReplicationFactor": replication_factor,
        "IamRoleArn": ROLE_ARN,
    })
}

/// The ARN a cluster of this account is addressed by.
fn cluster_arn(name: &str) -> String {
    format!("arn:aws:dax:{REGION}:{ACCOUNT}:cache/{name}")
}

/// Create the cluster `name` with `nodes` nodes and drive it to `available`.
async fn available_cluster(router: &MultiServiceRouter, name: &str, nodes: i64) -> Value {
    let (status, created) = call(router, "CreateCluster", create_cluster_body(name, nodes)).await;
    assert_eq!(status, 200, "CreateCluster {name}: got {status} {created:?}");
    cluster_in_status(router, name, state::CLUSTER_AVAILABLE).await
}

/// The `(Key, Value)` pairs a `Tags` list reports, in the order it reports them.
fn reported_tags(body: &Value) -> Vec<(String, String)> {
    body["Tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .map(|tag| {
                    (
                        tag["Key"].as_str().unwrap_or_default().to_string(),
                        tag["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `NodeId`s a describe view reports.
fn node_ids(cluster: &Value) -> Vec<String> {
    cluster["Nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| node["NodeId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule: the tags a `TagResource` stores on the cluster its `ResourceName` addresses are
// the tags `ListTags` reports, and a key the cluster already carries takes the new value
// rather than being stored twice.
// Source: `TagResource` ("Associates a set of tags with a DAX resource"), `ListTags` ("A
// list of tags currently associated with the DAX cluster") and `Tag$Key` ("Every DAX
// cluster can only have one tag with the same key. If you try to add an existing tag
// (same key), the existing tag value will be updated to the new value").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_tags_a_cluster_is_tagged_with_are_the_tags_it_lists() {
    let router = emulator();
    available_cluster(&router, "tagged", 1).await;
    let arn = cluster_arn("tagged");

    // Rows: one tag, several at once, a key already stored taking a new value, a tag with
    // an empty value ("Tag values ... can be null"), and a request repeating a key.
    let cases = [
        (
            "one tag",
            json!([{ "Key": "owner", "Value": "team" }]),
            vec![("owner", "team")],
        ),
        (
            "several at once",
            json!([
                { "Key": "env", "Value": "test" },
                { "Key": "cost-centre", "Value": "4711" },
            ]),
            vec![("owner", "team"), ("env", "test"), ("cost-centre", "4711")],
        ),
        (
            "a stored key takes the new value",
            json!([{ "Key": "env", "Value": "prod" }]),
            vec![("owner", "team"), ("env", "prod"), ("cost-centre", "4711")],
        ),
        (
            "an empty value",
            json!([{ "Key": "team", "Value": "" }]),
            vec![
                ("owner", "team"),
                ("env", "prod"),
                ("cost-centre", "4711"),
                ("team", ""),
            ],
        ),
        (
            "a key repeated in one request",
            json!([
                { "Key": "owner", "Value": "first" },
                { "Key": "owner", "Value": "last" },
            ]),
            vec![
                ("owner", "last"),
                ("env", "prod"),
                ("cost-centre", "4711"),
                ("team", ""),
            ],
        ),
    ];

    for (case, tags, expected) in cases {
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let (status, tagged) = call(
            &router,
            "TagResource",
            json!({ "ResourceName": arn, "Tags": tags }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: TagResource got {status} {tagged:?}"
        );
        assert_eq!(
            reported_tags(&tagged),
            expected,
            "case {case:?}: the answer must report the tag set as it now stands, \
             got {tagged:?}"
        );
        // Round trip: the stored tags are what a later caller lists.
        let (status, listed) = call(&router, "ListTags", json!({ "ResourceName": arn })).await;
        assert_eq!(status, 200, "case {case:?}: ListTags got {status} {listed:?}");
        assert_eq!(
            reported_tags(&listed),
            expected,
            "case {case:?}: the stored tags must read back, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a tag is stored UNDER its key, and what one tag may make this emulator store is
// bounded — a tag without a key, or a key or value longer than that bound, is refused with
// `InvalidParameterValueException` and nothing is stored, on BOTH paths that store tags.
// Source: `Tag$Key` ("The key for the tag ... Every DAX cluster can only have one tag
// with the same key"), with `state::MAX_TAG_KEY_LEN` / `state::MAX_TAG_VALUE_LEN` as the
// bound this emulator picks on the bytes one tag holds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tag_without_a_key_or_past_the_stored_bound_is_refused_on_every_path() {
    let router = emulator();
    available_cluster(&router, "bounded", 1).await;
    let arn = cluster_arn("bounded");

    // Rows: each way one tag fails, at its boundary — no key at all, an empty key, a key
    // one character past the bound, and a value one character past it.
    let cases = [
        ("no key member", json!({ "Value": "team" })),
        ("an empty key", json!({ "Key": "", "Value": "team" })),
        (
            "a key one character past the bound",
            json!({ "Key": "k".repeat(state::MAX_TAG_KEY_LEN + 1), "Value": "team" }),
        ),
        (
            "a value one character past the bound",
            json!({ "Key": "owner", "Value": "v".repeat(state::MAX_TAG_VALUE_LEN + 1) }),
        ),
    ];

    for (case, tag) in &cases {
        // The path that tags an existing cluster.
        let (status, answered) = call(
            &router,
            "TagResource",
            json!({ "ResourceName": arn, "Tags": [tag] }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?} on TagResource: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidParameterValueException",
            "case {case:?} on TagResource: expected InvalidParameterValueException, \
             got {answered:?}"
        );
        // Nothing was stored: the cluster still carries no tag.
        let (status, listed) = call(&router, "ListTags", json!({ "ResourceName": arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTags got {status} {listed:?}"
        );
        assert_eq!(
            reported_tags(&listed),
            Vec::<(String, String)>::new(),
            "case {case:?}: a refused tag must store nothing, got {listed:?}"
        );

        // And the path that tags a cluster as it is created.
        let mut body = create_cluster_body("tagged-at-create", 1);
        body["Tags"] = json!([tag]);
        let (status, answered) = call(&router, "CreateCluster", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?} on CreateCluster: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidParameterValueException",
            "case {case:?} on CreateCluster: expected InvalidParameterValueException, \
             got {answered:?}"
        );
        let (status, read) = call(
            &router,
            "DescribeClusters",
            json!({ "ClusterNames": ["tagged-at-create"] }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: a refused create must store no cluster, got {status} {read:?}"
        );
    }

    // A tag AT the bound is stored: the refusal is the boundary, not the size itself.
    let key = "k".repeat(state::MAX_TAG_KEY_LEN);
    let value = "v".repeat(state::MAX_TAG_VALUE_LEN);
    let (status, tagged) = call(
        &router,
        "TagResource",
        json!({ "ResourceName": arn, "Tags": [{ "Key": key, "Value": value }] }),
    )
    .await;
    assert_eq!(status, 200, "TagResource got {status} {tagged:?}");
    assert_eq!(
        reported_tags(&tagged),
        vec![(key, value)],
        "a tag at the bound is stored, got {tagged:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: an `UntagResource` removes exactly the keys it names and answers them; a key the
// cluster does not carry is refused with `TagNotFoundFault` and NOTHING is removed.
// Source: `UntagResource` ("Removes the association of tags from a DAX resource"), its
// `Tags` output ("The tag keys that have been removed from the cluster") and the modelled
// `TagNotFoundFault` ("The tag does not exist").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn untagging_removes_the_named_keys_and_refuses_a_key_the_cluster_does_not_carry() {
    let router = emulator();
    available_cluster(&router, "untagged", 1).await;
    let arn = cluster_arn("untagged");
    let (status, tagged) = call(
        &router,
        "TagResource",
        json!({
            "ResourceName": arn,
            "Tags": [
                { "Key": "a", "Value": "1" },
                { "Key": "b", "Value": "2" },
                { "Key": "c", "Value": "3" },
            ],
        }),
    )
    .await;
    assert_eq!(status, 200, "TagResource got {status} {tagged:?}");

    // Rows: a key nothing stored, one of a pair where the other IS stored (so the whole
    // request must be refused), and a key differing from a stored one only in case —
    // "Tag keys are case sensitive" (`Tag$Key`).
    let refused: [(&str, Vec<&str>); 3] = [
        ("a key nothing stored", vec!["absent"]),
        ("one absent among stored", vec!["a", "absent"]),
        ("a stored key in another casing", vec!["A"]),
    ];
    for (case, keys) in refused {
        let (status, answered) = call(
            &router,
            "UntagResource",
            json!({ "ResourceName": arn, "TagKeys": keys }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "TagNotFoundFault",
            "case {case:?}: expected TagNotFoundFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            format!(
                "Failing request. These tags are not present : {}",
                keys.iter()
                    .filter(|key| !["a", "b", "c"].contains(*key))
                    .copied()
                    .collect::<Vec<&str>>()
                    .join(", ")
            ),
            "case {case:?}: the fault must list the keys that are not present, got {answered:?}"
        );
        // Nothing was removed.
        let (status, listed) = call(&router, "ListTags", json!({ "ResourceName": arn })).await;
        assert_eq!(status, 200, "case {case:?}: ListTags got {status} {listed:?}");
        assert_eq!(
            reported_tags(&listed)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<String>>(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "case {case:?}: a refused untag must leave every tag in place, got {listed:?}"
        );
    }

    // Rows: one key, then the rest in one request.
    let removals: [(&str, Vec<&str>, Vec<&str>); 2] = [
        ("one key", vec!["b"], vec!["a", "c"]),
        ("the rest at once", vec!["a", "c"], vec![]),
    ];
    for (case, keys, remaining) in removals {
        let (status, answered) = call(
            &router,
            "UntagResource",
            json!({ "ResourceName": arn, "TagKeys": keys.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UntagResource got {status} {answered:?}"
        );
        assert_eq!(
            reported_tags(&answered)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<String>>(),
            keys.iter().map(|key| (*key).to_string()).collect::<Vec<String>>(),
            "case {case:?}: the answer reports the keys that were removed, got {answered:?}"
        );
        let (status, listed) = call(&router, "ListTags", json!({ "ResourceName": arn })).await;
        assert_eq!(status, 200, "case {case:?}: ListTags got {status} {listed:?}");
        assert_eq!(
            reported_tags(&listed)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<String>>(),
            remaining
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: only the keys that were not removed read back, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a `ResourceName` is read as an ARN of this service naming a cluster — a value
// that is not one is refused with `InvalidARNFault`, whichever tagging operation carries
// it, and nothing is read or written.
// Source: the modelled `InvalidARNFault` ("The Amazon Resource Name (ARN) supplied in the
// request is not valid") and `cluster_arn`'s own form,
// `arn:aws:dax:<region>:<account>:cache/<ClusterName>`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_resource_name_that_is_not_a_dax_cluster_arn_is_refused() {
    let router = emulator();
    available_cluster(&router, "arn-cluster", 1).await;

    // Rows: each way a value fails to be a DAX cluster ARN, with the refusal it draws —
    // a value that cannot be read as an ARN at all is refused on its component count, and
    // one that can but names another resource on the modelled sentence for the fault.
    let component_count = "Input ARN string does not have 7 components.";
    let not_this_services = "The Amazon Resource Name (ARN) supplied in the request is not valid.";
    let cases = [
        (
            "a bare cluster name",
            "arn-cluster".to_string(),
            component_count.to_string(),
        ),
        (
            "too few fields",
            format!("arn:aws:dax:{REGION}:cache/arn-cluster"),
            component_count.to_string(),
        ),
        (
            "the empty string",
            String::new(),
            component_count.to_string(),
        ),
        (
            "another vendor",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:cache/arn-cluster"),
            not_this_services.to_string(),
        ),
        (
            "another resource type",
            format!("arn:aws:dax:{REGION}:{ACCOUNT}:subnetgroup/arn-cluster"),
            not_this_services.to_string(),
        ),
        (
            "no cluster id",
            format!("arn:aws:dax:{REGION}:{ACCOUNT}:cache/"),
            not_this_services.to_string(),
        ),
    ];

    for (case, resource_name, expected) in cases {
        for operation in ["ListTags", "TagResource", "UntagResource"] {
            let body = match operation {
                "TagResource" => json!({
                    "ResourceName": resource_name,
                    "Tags": [{ "Key": "owner", "Value": "team" }],
                }),
                "UntagResource" => {
                    json!({ "ResourceName": resource_name, "TagKeys": ["owner"] })
                }
                _ => json!({ "ResourceName": resource_name }),
            };
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                status, CLIENT_FAULT_STATUS,
                "case {case:?} on {operation}: expected a refusal, got {status} {answered:?}"
            );
            assert_eq!(
                error_code(&answered),
                "InvalidARNFault",
                "case {case:?} on {operation}: expected InvalidARNFault, got {answered:?}"
            );
            assert_eq!(
                error_message(&answered),
                expected,
                "case {case:?} on {operation}: the refusal must state why the value is not \
                 a DAX cluster ARN, got {answered:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule: `RebootNode` restarts ONE node of an available cluster — "During the reboot, the
// node status is set to REBOOTING" — keeping the node, its id and its endpoint, and the
// cluster is settled back to `available` by the clock. A `NodeId` the cluster carries no
// node under is refused with `NodeNotFoundFault`, changing nothing.
// Source: `RebootNode` ("RebootNode restarts the DAX engine process and does not remove
// the contents of the cache") and the modelled `NodeNotFoundFault` ("None of the nodes in
// the cluster have the given node ID").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rebooting_a_node_keeps_it_and_settles_the_cluster_back_to_available() {
    let router = emulator();
    let cluster = available_cluster(&router, "rebooted", 3).await;
    let before = node_ids(&cluster);
    assert_eq!(
        before,
        vec![
            "rebooted-a".to_string(),
            "rebooted-b".to_string(),
            "rebooted-c".to_string()
        ],
        "a node is identified by its cluster and a letter, got {cluster:?}"
    );

    // Rows: every node of the cluster, so which node is rebooted is the request's choice.
    for node_id in &before {
        let (status, answered) = call(
            &router,
            "RebootNode",
            json!({ "ClusterName": "rebooted", "NodeId": node_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {node_id:?}: RebootNode got {status} {answered:?}"
        );
        let rebooting = &answered["Cluster"];
        assert_eq!(
            rebooting["Status"],
            json!(state::CLUSTER_MODIFYING),
            "case {node_id:?}: the cluster applies the reboot, got {rebooting:?}"
        );
        let rebooting_node = rebooting["Nodes"]
            .as_array()
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|node| node["NodeId"] == json!(node_id))
                    .cloned()
            })
            .unwrap_or(Value::Null);
        assert_eq!(
            rebooting_node["NodeStatus"],
            json!(state::NODE_REBOOTING),
            "case {node_id:?}: the rebooting node reports it, got {rebooting_node:?}"
        );

        // The clock settles it: the same nodes, every one available again.
        let settled = cluster_in_status(&router, "rebooted", state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            node_ids(&settled),
            before,
            "case {node_id:?}: a reboot removes no node, got {settled:?}"
        );
        assert_eq!(
            settled["ActiveNodes"],
            json!(3),
            "case {node_id:?}: every settled node serves requests again, got {settled:?}"
        );
    }

    // Rows: a node id of another cluster's form, one past the cluster's letters, and a
    // bare letter.
    for node_id in ["other-a", "rebooted-z", "a"] {
        let (status, answered) = call(
            &router,
            "RebootNode",
            json!({ "ClusterName": "rebooted", "NodeId": node_id }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {node_id:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "NodeNotFoundFault",
            "case {node_id:?}: expected NodeNotFoundFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            "Node not found.",
            "case {node_id:?}: the fault's template names no identifier, got {answered:?}"
        );
        let unchanged = cluster_in_status(&router, "rebooted", state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            node_ids(&unchanged),
            before,
            "case {node_id:?}: a refused reboot changes nothing, got {unchanged:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: `DecreaseReplicationFactor` gives up nodes until the cluster holds exactly
// `NewReplicationFactor` of them — the ones `NodeIdsToRemove` names first, then the most
// recently added of the rest — and the clock removes them, so the cluster reports them in
// `NodeIdsToRemove` until it settles back to `available` holding the smaller node list. A
// factor below 1 or at/above the current count is refused with
// `InvalidParameterValueException`, a node id the cluster does not carry with
// `NodeNotFoundFault`, and a `NodeIdsToRemove` naming more nodes than the cluster is
// giving up with `InvalidParameterCombinationException`.
// Source: `DecreaseReplicationFactor` ("Removes one or more nodes from a DAX cluster. You
// cannot use DecreaseReplicationFactor to remove the last node in a DAX cluster. If you
// need to do this, use DeleteCluster instead"), its `NodeIdsToRemove` ("The unique
// identifiers of the nodes to be removed from the cluster") and `Cluster$NodeIdsToRemove`
// ("A list of nodes to be removed from the cluster").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn decreasing_the_replication_factor_removes_the_nodes_the_request_gives_up() {
    let router = emulator();

    // Rows differ along what the rule generalises over: how many nodes go, and whether
    // the request names WHICH ones.
    let cases = [
        ("one node, unnamed", "shrink-one", 3_i64, 2_i64, vec![], vec!["a", "b"]),
        ("one node, named", "shrink-named", 3, 2, vec!["a"], vec!["b", "c"]),
        (
            "two nodes, named",
            "shrink-two",
            4,
            2,
            vec!["b", "d"],
            vec!["a", "c"],
        ),
        (
            "one node, named among two given up",
            "shrink-partly-named",
            3_i64,
            1_i64,
            vec!["a"],
            vec!["b"],
        ),
        ("down to one node", "shrink-last", 3, 1, vec![], vec!["a"]),
    ];

    for (case, name, created, requested, named, remaining) in cases {
        available_cluster(&router, name, created).await;
        let mut body = json!({ "ClusterName": name, "NewReplicationFactor": requested });
        if !named.is_empty() {
            body["NodeIdsToRemove"] = json!(named
                .iter()
                .map(|suffix| format!("{name}-{suffix}"))
                .collect::<Vec<String>>());
        }
        let (status, answered) = call(&router, "DecreaseReplicationFactor", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DecreaseReplicationFactor got {status} {answered:?}"
        );
        let giving_up = answered["Cluster"]["NodeIdsToRemove"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            giving_up,
            usize::try_from(created - requested).unwrap(),
            "case {case:?}: the nodes being given up must be named, got {answered:?}"
        );
        assert_eq!(
            answered["Cluster"]["TotalNodes"],
            json!(created),
            "case {case:?}: a node is reported until the clock removes it, got {answered:?}"
        );

        let settled = cluster_in_status(&router, name, state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            settled["TotalNodes"],
            json!(requested),
            "case {case:?}: the settled cluster holds the node count asked for, got {settled:?}"
        );
        assert_eq!(
            settled["NodeIdsToRemove"],
            Value::Null,
            "case {case:?}: no removal is pending once the clock ran it, got {settled:?}"
        );
        assert_eq!(
            node_ids(&settled),
            remaining
                .iter()
                .map(|suffix| format!("{name}-{suffix}"))
                .collect::<Vec<String>>(),
            "case {case:?}: the nodes given up are gone from the store, got {settled:?}"
        );
        assert_eq!(
            settled["ActiveNodes"],
            json!(requested),
            "case {case:?}: every node that stayed serves requests, got {settled:?}"
        );
    }

    // The refusals, each against a settled three-node cluster.
    available_cluster(&router, "refused-shrink", 3).await;
    let cases: [(&str, Value, &str); 5] = [
        (
            "the last node",
            json!({ "ClusterName": "refused-shrink", "NewReplicationFactor": 0 }),
            "InvalidParameterValueException",
        ),
        (
            "the current count",
            json!({ "ClusterName": "refused-shrink", "NewReplicationFactor": 3 }),
            "InvalidParameterValueException",
        ),
        (
            "more nodes than it holds",
            json!({ "ClusterName": "refused-shrink", "NewReplicationFactor": 5 }),
            "InvalidParameterValueException",
        ),
        (
            "a node it does not carry",
            json!({
                "ClusterName": "refused-shrink",
                "NewReplicationFactor": 2,
                "NodeIdsToRemove": ["refused-shrink-z"],
            }),
            "NodeNotFoundFault",
        ),
        (
            "more node ids than nodes given up",
            json!({
                "ClusterName": "refused-shrink",
                "NewReplicationFactor": 2,
                "NodeIdsToRemove": ["refused-shrink-a", "refused-shrink-b"],
            }),
            "InvalidParameterCombinationException",
        ),
    ];

    for (case, body, expected) in cases {
        let (status, answered) = call(&router, "DecreaseReplicationFactor", body).await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {case:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            expected,
            "case {case:?}: expected {expected}, got {answered:?}"
        );
        let unchanged = cluster_in_status(&router, "refused-shrink", state::CLUSTER_AVAILABLE).await;
        assert_eq!(
            unchanged["TotalNodes"],
            json!(3),
            "case {case:?}: a refused decrease removes no node, got {unchanged:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a subnet group a stored cluster is placed in cannot be removed — "You cannot
// delete a subnet group if it is associated with any DAX clusters" — so the delete is
// refused with `SubnetGroupInUseFault` and the group stays; once no cluster is placed in
// it the delete removes it and a describe no longer reports it. A name this account holds
// no group under is answered with `SubnetGroupNotFoundFault`.
// Source: `DeleteSubnetGroup` and the modelled `SubnetGroupInUseFault` ("The specified
// subnet group is currently in use").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subnet_group_a_cluster_is_placed_in_cannot_be_deleted() {
    let router = emulator();
    let subnets = account_subnet_ids(&router, REGION).await;
    for group in ["in-use", "free"] {
        let (status, created) = call(
            &router,
            "CreateSubnetGroup",
            json!({ "SubnetGroupName": group, "SubnetIds": [subnets[0], subnets[1]] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateSubnetGroup {group}: got {status} {created:?}"
        );
    }
    let mut body = create_cluster_body("placed", 1);
    body["SubnetGroupName"] = json!("in-use");
    let (status, created) = call(&router, "CreateCluster", body).await;
    assert_eq!(status, 200, "CreateCluster: got {status} {created:?}");
    cluster_in_status(&router, "placed", state::CLUSTER_AVAILABLE).await;

    // Rows: the group the cluster is placed in, named as stored and in another casing —
    // the name "is stored as a lowercase string", so both address the same group.
    for named in ["in-use", "IN-USE"] {
        let (status, answered) = call(
            &router,
            "DeleteSubnetGroup",
            json!({ "SubnetGroupName": named }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {named:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "SubnetGroupInUseFault",
            "case {named:?}: expected SubnetGroupInUseFault, got {answered:?}"
        );
        assert_eq!(
            error_message(&answered),
            "Subnet group in-use is currently in use by a cluster.",
            "case {named:?}: the fault must name the group as stored, got {answered:?}"
        );
        let (status, described) = call(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": ["in-use"] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {named:?}: a refused delete keeps the group, got {status} {described:?}"
        );
    }

    // A group no cluster is placed in IS removed, and the describe stops reporting it.
    let (status, deleted) = call(
        &router,
        "DeleteSubnetGroup",
        json!({ "SubnetGroupName": "free" }),
    )
    .await;
    assert_eq!(status, 200, "DeleteSubnetGroup free: got {status} {deleted:?}");
    assert!(
        deleted["DeletionMessage"]
            .as_str()
            .unwrap_or_default()
            .contains("free"),
        "the deletion message must name the group that was removed, got {deleted:?}"
    );
    let (status, described) = call(
        &router,
        "DescribeSubnetGroups",
        json!({ "SubnetGroupNames": ["free"] }),
    )
    .await;
    assert_eq!(
        status, CLIENT_FAULT_STATUS,
        "a removed group is gone: got {status} {described:?}"
    );
    assert_eq!(
        error_code(&described),
        "SubnetGroupNotFoundFault",
        "expected SubnetGroupNotFoundFault, got {described:?}"
    );

    // And a second delete of it, like a delete of a name nothing created, is a not-found.
    for group in ["free", "never-created"] {
        let (status, answered) = call(
            &router,
            "DeleteSubnetGroup",
            json!({ "SubnetGroupName": group }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {group:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "SubnetGroupNotFoundFault",
            "case {group:?}: expected SubnetGroupNotFoundFault, got {answered:?}"
        );
    }

    // Once the cluster is gone, the group it was placed in is free to go too.
    let (status, answered) = call(&router, "DeleteCluster", json!({ "ClusterName": "placed" })).await;
    assert_eq!(status, 200, "DeleteCluster: got {status} {answered:?}");
    // Look again until the clock has removed the cluster, rather than waiting a fixed
    // time for it.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) = call(
            &router,
            "DescribeClusters",
            json!({ "ClusterNames": ["placed"] }),
        )
        .await;
        if status == CLIENT_FAULT_STATUS && error_code(&described) == "ClusterNotFoundFault" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the cluster was never removed; the last DescribeClusters answered \
             {status} {described:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
    let (status, group_freed) = call(
        &router,
        "DeleteSubnetGroup",
        json!({ "SubnetGroupName": "in-use" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group a removed cluster was placed in is free to go, got {status} {group_freed:?}"
    );
    assert!(
        group_freed["DeletionMessage"]
            .as_str()
            .unwrap_or_default()
            .contains("in-use"),
        "the deletion message must name the group that was removed, got {group_freed:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: `ListTags` issues no continuation token — every tag of a cluster fits in one page
// — so a `NextToken` it did not issue is refused with `InvalidParameterValueException`.
// Source: `ListTags$NextToken` ("An optional token returned from a prior request") and
// `Tag` ("You can add up to 50 tags to a single DAX cluster").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_list_tags_token_this_service_did_not_issue_is_refused() {
    let router = emulator();
    available_cluster(&router, "paged", 1).await;
    let arn = cluster_arn("paged");
    let (status, listed) = call(&router, "ListTags", json!({ "ResourceName": arn })).await;
    assert_eq!(status, 200, "ListTags got {status} {listed:?}");
    assert_eq!(
        listed["NextToken"],
        Value::Null,
        "one page holds every tag, so no token is issued, got {listed:?}"
    );

    // Rows: a token that is not one of this service's at all, and one shaped like a
    // continuation of another operation's page.
    for token in ["not-a-token", "6162636465"] {
        let (status, answered) = call(
            &router,
            "ListTags",
            json!({ "ResourceName": arn, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, CLIENT_FAULT_STATUS,
            "case {token:?}: expected a refusal, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidParameterValueException",
            "case {token:?}: expected InvalidParameterValueException, got {answered:?}"
        );
    }

    // A cluster the account does not hold is a not-found: the ARN resolves against the
    // store, not against the request.
    let (status, answered) = call(
        &router,
        "ListTags",
        json!({ "ResourceName": cluster_arn("never-created") }),
    )
    .await;
    assert_eq!(
        status, CLIENT_FAULT_STATUS,
        "a cluster nothing created is a not-found, got {status} {answered:?}"
    );
    assert_eq!(
        error_code(&answered),
        "ClusterNotFoundFault",
        "expected ClusterNotFoundFault, got {answered:?}"
    );
}
