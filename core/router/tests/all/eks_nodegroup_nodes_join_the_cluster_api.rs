// SPDX-License-Identifier: CC0-1.0

//! A managed node group's instances JOIN the cluster's Kubernetes API, and a live
//! node group blocks the delete of the cluster that holds it.
//!
//! The RULES under test:
//!
//! * the nodes a cluster's Kubernetes API server reports are the Amazon EC2
//!   instances its ACTIVE managed node groups launched: one `Node` per instance the
//!   node group's Auto Scaling group really holds, whose `spec.providerID` names
//!   that instance, whose `status.conditions` carry `{type: Ready, status: "True"}`
//!   — a node of an ACTIVE node group has registered — and whose labels name the
//!   node group it belongs to (`eks.amazonaws.com/nodegroup`), so a client that
//!   asks for the nodes of one node group is answered its nodes and no others;
//! * a cluster with a dependent node group cannot be deleted: `DeleteCluster` is
//!   refused 409 `ResourceInUseException` with the sentence
//!   `Cluster has nodegroups attached`, leaving the cluster and the node group
//!   ACTIVE and the node still Ready, and the identical call is accepted with
//!   `cluster.status` `DELETING` once the node group is gone.
//!
//! This suite lives here rather than in the EKS crate because a node is an
//! instance of ANOTHER service: Amazon EKS creates the node group's capacity in
//! Amazon EC2 Auto Scaling and Amazon EC2 and reads its instances back from them,
//! so what the cluster reports as a node is only real in the assembled emulator —
//! the one the production binary mounts. The rows differ along how many nodes a
//! node group was created with, which is what the first rule generalises over.
//!
//! The control-plane harness is the test crate's own (`eks_wire`); the Kubernetes
//! half is one HTTP/1.1 exchange on the port the cluster published, so nothing but
//! the address the emulator announced is used to reach it.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::eks_wire::{autoscaling, cluster_body, ec2, eks, role_arn, Answer};
use crate::query_wire::{elements, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How often a clock-owed read is looked at again, and how many times in all:
/// nothing inside the emulator sleeps for a due instant, so a condition is polled
/// rather than waited for, with a deadline far above every span this service owes.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 400 looks at 50ms is a 20-second deadline.
const POLLS: usize = 400;

/// The label every Amazon EKS node carries naming the node group it belongs to.
const NODEGROUP_LABEL: &str = "eks.amazonaws.com/nodegroup";

/// Poll `read` until `holds` answers true, and answer the last read; panic naming
/// `what` when the deadline passes first.
fn read_until(
    what: &str,
    mut read: impl FnMut() -> Answer,
    holds: impl Fn(&Answer) -> bool,
) -> Answer {
    let mut last = read();
    for _ in 0..POLLS {
        if holds(&last) {
            return last;
        }
        std::thread::sleep(POLL_INTERVAL);
        last = read();
    }
    panic!("expected {what}; the last read answered {last:?}");
}

/// One answer from the cluster's published Kubernetes API address.
#[derive(Debug)]
struct ApiAnswer {
    status: u16,
    body: Value,
}

/// One request to that address, the way a kubeconfig-built client makes it: one
/// HTTP/1.1 exchange on the port the cluster published, presenting the ACCOUNT's
/// own credential — the principal that created the cluster and administers it.
/// An API server authenticates every request, so a client of one presents a token.
fn k8s(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    method: &str,
    path: &str,
) -> ApiAnswer {
    let authority = endpoint
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("{what}: expected an https endpoint, got {endpoint:?}"))
        .to_string();
    let (_, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("{what}: the endpoint must carry a port, got {authority:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("{what}: {port:?} must be a port: {error}"));
    runtime.block_on(async move {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap_or_else(|error| {
                panic!("{what}: the published address must accept: {error}")
            });
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            crate::eks_wire::account_kubernetes_bearer_token()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .unwrap_or_else(|error| panic!("{what}: the request must be written: {error}"));
        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .await
            .unwrap_or_else(|error| panic!("{what}: the answer must be read: {error}"));
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, answered) = text
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("{what}: expected an HTTP answer, got {text:?}"));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("{what}: expected a status line, got {head:?}"));
        ApiAnswer {
            status,
            body: serde_json::from_str(answered).unwrap_or(Value::Null),
        }
    })
}

/// The nodes of `nodegroup` the cluster reports as READY, as a client asks for
/// them: the node collection restricted to the node group's own label, keeping
/// the nodes whose `Ready` condition is `True`.
///
/// The RESTRICTION is the server's: every node the answer carries must be one of
/// that node group's, so a client asking for one node group's nodes is answered
/// its nodes and no others.
fn ready_nodes(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    nodegroup: &str,
) -> Vec<Value> {
    let answered = k8s(
        runtime,
        endpoint,
        what,
        "GET",
        &format!("/api/v1/nodes?labelSelector={NODEGROUP_LABEL}%3D{nodegroup}"),
    );
    assert_eq!(
        answered.status, 200,
        "{what}: the node collection must be served, got {answered:?}"
    );
    let items = answered.body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for node in &items {
        assert_eq!(
            node["metadata"]["labels"][NODEGROUP_LABEL],
            serde_json::json!(nodegroup),
            "{what}: a node the selector asked for must belong to {nodegroup:?}, got {node:?}"
        );
    }
    items
        .into_iter()
        .filter(|node| {
            node["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| {
                    conditions.iter().any(|condition| {
                        condition["type"] == serde_json::json!("Ready")
                            && condition["status"] == serde_json::json!("True")
                    })
                })
        })
        .collect()
}

/// Wait until the cluster reports exactly `wanted` Ready nodes of `nodegroup`, and
/// answer them.
fn ready_nodes_until(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    nodegroup: &str,
    wanted: usize,
) -> Vec<Value> {
    for _ in 0..POLLS {
        let nodes = ready_nodes(runtime, endpoint, what, nodegroup);
        if nodes.len() == wanted {
            return nodes;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!(
        "{what}: expected {wanted} Ready node(s) of {nodegroup:?}, got {:?}",
        ready_nodes(runtime, endpoint, what, nodegroup)
    );
}

/// The instance ids the Auto Scaling group `group_name` holds, as Auto Scaling
/// itself reports them.
fn group_instance_ids(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group_name: &str,
) -> Vec<String> {
    let described = autoscaling(
        router,
        runtime,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group_name)],
    );
    assert_eq!(
        described.status, 200,
        "Auto Scaling must answer for {group_name:?}, got {described:?}"
    );
    elements(&described.text, "InstanceId")
}

/// The account's own default subnets, read from EC2 rather than invented.
fn default_subnets(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Vec<String> {
    let defaults = ec2(
        router,
        runtime,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    );
    assert_eq!(
        defaults.status, 200,
        "setup: DescribeSubnets must succeed, got {defaults:?}"
    );
    let available = elements(&defaults.text, "subnetId");
    assert!(
        available.len() >= 2,
        "setup: the account must have default subnets, got {available:?}"
    );
    vec![available[0].clone(), available[1].clone()]
}

#[test]
fn an_active_node_groups_instances_are_ready_nodes_and_block_the_clusters_delete() {
    std::thread::Builder::new()
        .name("eks-nodegroup-nodes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            let subnets = default_subnets(&router, &runtime);
            let cluster = "nodegroup-nodes-host";
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &subnets)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster must be created, got {created:?} ({})",
                created.message()
            );
            let active = read_until(
                "the cluster to reach ACTIVE with its API server address published",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| {
                    answer.body["cluster"]["status"] == serde_json::json!("ACTIVE")
                        && answer.body["cluster"]["endpoint"].is_string()
                },
            );
            let endpoint = active.body["cluster"]["endpoint"]
                .as_str()
                .expect("the published API server address")
                .to_string();

            // The rows differ along how many nodes the node group was created with:
            // one node per instance its Auto Scaling group holds, whichever number
            // that is.
            let rows: [(&str, &str, i64); 2] = [
                ("a node group of one node", "ng-single", 1),
                ("a node group of two nodes", "ng-pair", 2),
            ];
            let mut groups: Vec<(&str, String)> = Vec::new();
            for (case, nodegroup, desired) in rows {
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/clusters/{cluster}/node-groups"),
                    Some(serde_json::json!({
                        "nodegroupName": nodegroup,
                        "nodeRole": role_arn("eksNodeRole"),
                        "subnets": subnets,
                        "scalingConfig": {
                            "minSize": desired,
                            "maxSize": desired,
                            "desiredSize": desired,
                        },
                    })),
                );
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: the node group must be created, got {answer:?} ({})",
                    answer.message()
                );
                let settled = read_until(
                    "the node group to reach ACTIVE",
                    || {
                        eks(
                            &router,
                            &runtime,
                            "GET",
                            &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
                            None,
                        )
                    },
                    |answer| answer.body["nodegroup"]["status"] == serde_json::json!("ACTIVE"),
                );
                let group_name = settled.body["nodegroup"]["resources"]["autoScalingGroups"][0]
                    ["name"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the node group must report its Auto Scaling group, got {}", settled.text)
                    })
                    .to_string();
                groups.push((nodegroup, group_name.clone()));

                // THE NODES ARE THE GROUP'S OWN INSTANCES: as many Ready nodes as
                // Auto Scaling holds instances, each naming one of them.
                let instance_ids = group_instance_ids(&router, &runtime, &group_name);
                assert_eq!(
                    instance_ids.len(),
                    usize::try_from(desired).unwrap_or_default(),
                    "case {case:?}: Auto Scaling must hold the node group's instances, got \
                     {instance_ids:?}"
                );
                let nodes =
                    ready_nodes_until(&runtime, &endpoint, case, nodegroup, instance_ids.len());
                for instance_id in &instance_ids {
                    let node = nodes
                        .iter()
                        .find(|node| {
                            node["spec"]["providerID"]
                                .as_str()
                                .is_some_and(|provider| provider.ends_with(instance_id))
                        })
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: a Ready node must name the instance \
                                 {instance_id:?}, got {nodes:?}"
                            )
                        });
                    assert_eq!(
                        node["metadata"]["labels"][NODEGROUP_LABEL],
                        serde_json::json!(nodegroup),
                        "case {case:?}: the node must name the node group it belongs to, got \
                         {node:?}"
                    );
                    assert!(
                        node["metadata"]["name"]
                            .as_str()
                            .is_some_and(|name| !name.is_empty()),
                        "case {case:?}: the node must register under a name, got {node:?}"
                    );
                }
            }

            // The whole collection carries every node group's nodes, and a
            // selector for a label no node of the cluster carries asks for none:
            // the restriction is the SELECTOR's, not the collection's.
            let whole = k8s(
                &runtime,
                &endpoint,
                "the whole node collection",
                "GET",
                "/api/v1/nodes",
            );
            assert_eq!(
                whole.status, 200,
                "the node collection must be served, got {whole:?}"
            );
            assert_eq!(
                whole.body["items"].as_array().map(Vec::len),
                Some(3),
                "every node of every node group must be in the unrestricted collection, got {}",
                whole.body
            );
            let none = k8s(
                &runtime,
                &endpoint,
                "a selector no node satisfies",
                "GET",
                &format!("/api/v1/nodes?labelSelector={NODEGROUP_LABEL}%3Dng-absent"),
            );
            assert_eq!(
                (none.status, none.body["items"].as_array().map(Vec::len)),
                (200, Some(0)),
                "a selector no node satisfies must ask for none, got {none:?}"
            );

            // A LIVE DEPENDENT BLOCKS THE DELETE: the cluster still holds both node
            // groups, so the delete is refused with the sentence AWS words it with
            // and nothing moves.
            let refused = eks(
                &router,
                &runtime,
                "DELETE",
                &format!("/clusters/{cluster}"),
                None,
            );
            assert_eq!(
                (refused.status, refused.error_type.as_deref()),
                (409, Some("ResourceInUseException")),
                "a cluster with a node group attached must be refused 409 \
                 ResourceInUseException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                "Cluster has nodegroups attached",
                "expected AWS's sentence, got {refused:?}"
            );
            let unchanged = eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None);
            assert_eq!(
                unchanged.body["cluster"]["status"],
                serde_json::json!("ACTIVE"),
                "a refused delete must leave the cluster ACTIVE, got {}",
                unchanged.text
            );
            for (nodegroup, _) in &groups {
                let still_there = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
                    None,
                );
                assert_eq!(
                    still_there.body["nodegroup"]["status"],
                    serde_json::json!("ACTIVE"),
                    "a refused delete must leave {nodegroup:?} ACTIVE, got {}",
                    still_there.text
                );
                assert!(
                    !ready_nodes(&runtime, &endpoint, "after the refused delete", nodegroup)
                        .is_empty(),
                    "a refused delete must leave {nodegroup:?}'s nodes Ready"
                );
            }

            // Once every node group is gone — and with it every node it registered —
            // the identical call is accepted and the cluster reports DELETING.
            for (nodegroup, group_name) in &groups {
                let deleted = eks(
                    &router,
                    &runtime,
                    "DELETE",
                    &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
                    None,
                );
                assert_eq!(
                    deleted.status, 200,
                    "the node group {nodegroup:?} must be deleted, got {deleted:?} ({})",
                    deleted.message()
                );
                read_until(
                    "the node group to be reported absent once its removal has finished",
                    || {
                        eks(
                            &router,
                            &runtime,
                            "GET",
                            &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
                            None,
                        )
                    },
                    |answer| answer.status == 404,
                );
                let released = autoscaling(
                    &router,
                    &runtime,
                    "DescribeAutoScalingGroups",
                    &[("AutoScalingGroupNames.member.1", group_name.as_str())],
                );
                assert_eq!(
                    opt_element(&released.text, "AutoScalingGroupName"),
                    None,
                    "the deleted node group's Auto Scaling group must be released, got {}",
                    released.text
                );
                assert!(
                    ready_nodes(&runtime, &endpoint, "after the node group's delete", nodegroup)
                        .is_empty(),
                    "a deleted node group must register no node"
                );
            }

            let accepted = eks(
                &router,
                &runtime,
                "DELETE",
                &format!("/clusters/{cluster}"),
                None,
            );
            assert_eq!(
                accepted.status, 200,
                "the cluster must be deleted once its node groups are gone, got {accepted:?} \
                 ({})",
                accepted.message()
            );
            assert_eq!(
                accepted.body["cluster"]["status"],
                serde_json::json!("DELETING"),
                "the accepted delete must report the cluster DELETING, got {}",
                accepted.text
            );
        })
        .expect("spawn the eks nodegroup-nodes thread")
        .join()
        .expect("the eks nodegroup-nodes thread");
}
