// SPDX-License-Identifier: CC0-1.0

//! A pod's ADDRESS is an address of the VPC its node runs in: the address the
//! cluster's networking plugin took on that node's own elastic network interface.
//!
//! The RULE under test: **a pod the cluster has placed on a node holds one address
//! of the subnet that node's primary elastic network interface sits in, recorded as
//! a SECONDARY private address on that very interface — so the address the pod
//! reports as `status.podIP` (and as the one entry of `status.podIPs`) is an address
//! Amazon EC2's own `DescribeNetworkInterfaces` reports for that interface with
//! `primary` false, it lies inside the subnet's IPv4 CIDR block, it is not the
//! node's own address and no two pods hold the same one; a pod the cluster could
//! place on no node holds none; and a pod that is gone gives its address back, so
//! the interface no longer reports it.**
//!
//! Two crates have to agree for that to hold — Amazon EKS serves the cluster's
//! Kubernetes API and places the pod, while Amazon EC2 owns the instance the node
//! is, its interface, its subnet and the pool every address of that subnet comes out
//! of — so the suite lives here rather than in either of them: what a pod reports as
//! its address is only real in the assembled emulator.
//!
//! The rows differ along what the rule generalises over: HOW MANY pods one node
//! holds (the first, and each further one), and whether the cluster could place the
//! pod at all (a pod that names a node the cluster does not hold is placed nowhere).
//! The release half is driven through a workload whose replica count SHRINKS, which
//! is how a pod of this API leaves its node.
//!
//! The control-plane harness is the test crate's own (`eks_wire`); the Kubernetes
//! half is one HTTP/1.1 exchange on the port the cluster published, so nothing but
//! the address the emulator announced is used to reach it.

use std::collections::BTreeMap;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::eks_wire::{cluster_body, ec2, eks, role_arn, Answer};
use crate::query_wire::{block_contains, block_of, element, elements, items_of, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How often a clock-owed read is looked at again, and how many times in all:
/// nothing inside the emulator sleeps for a due instant, so a condition is polled
/// rather than waited for, with a deadline far above every span this emulator owes.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 600 looks at 50ms is a 30-second deadline.
const POLLS: usize = 600;

/// The node group every pod of this suite is placed on.
const NODEGROUP: &str = "addressed";

/// The image the pods run. What is under test is the ADDRESS a pod holds, not what
/// a registry answers for a reference, so the reference names a registry this
/// emulator does not hold — about which nothing is decided.
const IMAGE: &str = "registry.example.com/app:1";

/// One answer from a cluster's published Kubernetes API address.
#[derive(Debug)]
struct ApiAnswer {
    status: u16,
    body: Value,
}

/// One request to that address, the way a kubeconfig-built client makes it: one
/// HTTP/1.1 exchange on the port the cluster published, presenting the ACCOUNT's own
/// credential — the principal that created the cluster. An API server authenticates
/// every request, so a client of one presents a token.
fn k8s(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
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
    let payload = body.map(Value::to_string).unwrap_or_default();
    runtime.block_on(async move {
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap_or_else(|error| panic!("{what}: the published address must accept: {error}"));
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n\
             {payload}",
            crate::eks_wire::account_kubernetes_bearer_token(),
            payload.len()
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

/// Poll the cluster's API server until `holds` answers true of what `path` reports,
/// and answer that document; panic naming `what` when the deadline passes first.
fn api_until(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    path: &str,
    holds: impl Fn(&Value) -> bool,
) -> Value {
    let mut last = k8s(runtime, endpoint, what, "GET", path, None);
    for _ in 0..POLLS {
        if last.status == 200 && holds(&last.body) {
            return last.body;
        }
        std::thread::sleep(POLL_INTERVAL);
        last = k8s(runtime, endpoint, what, "GET", path, None);
    }
    panic!("expected {what}; the last read answered {last:?}");
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

/// The IPv4 CIDR block the subnet `subnet_id` was created with, as Amazon EC2
/// reports it.
fn subnet_block(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    subnet_id: &str,
) -> String {
    let described = ec2(
        router,
        runtime,
        "DescribeSubnets",
        &[("SubnetId.1", subnet_id)],
    );
    assert_eq!(
        described.status, 200,
        "setup: DescribeSubnets must answer for {subnet_id}, got {described:?}"
    );
    element(&described.text, "cidrBlock")
}

/// One node's primary elastic network interface, as Amazon EC2 reports it: its id,
/// the subnet it sits in, and every private IPv4 address it holds paired with
/// whether it holds it as its primary one.
#[derive(Debug)]
struct NodeInterface {
    network_interface_id: String,
    subnet_id: String,
    addresses: BTreeMap<String, bool>,
}

/// The interface attached to the instance `instance_id`, read through the documented
/// filter that selects an interface by the instance it is attached to.
fn node_interface(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    what: &str,
    instance_id: &str,
) -> NodeInterface {
    let described = ec2(
        router,
        runtime,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "attachment.instance-id"),
            ("Filter.1.Value.1", instance_id),
        ],
    );
    assert_eq!(
        described.status, 200,
        "{what}: DescribeNetworkInterfaces must answer for {instance_id}, got {described:?}"
    );
    let set = block_of(&described.text, "networkInterfaceSet").unwrap_or_else(|| {
        panic!(
            "{what}: Amazon EC2 must report an interface attached to {instance_id}, got {}",
            described.text
        )
    });
    // Read the ONE interface the filter selected rather than cutting the answer into
    // items: an ec2Query answer nests `<item>` lists inside each interface (its
    // groups, its addresses), so a reader over the items of the whole set would tear
    // one interface into several.
    let reported = elements(set, "networkInterfaceId");
    assert_eq!(
        reported.len(),
        1,
        "{what}: expected exactly one interface attached to {instance_id}, got {reported:?}"
    );
    let addresses_set = block_of(set, "privateIpAddressesSet").unwrap_or_else(|| {
        panic!("{what}: the interface of {instance_id} must report its addresses, got {set}")
    });
    let addresses = items_of(addresses_set)
        .into_iter()
        .filter_map(|entry| {
            let address = opt_element(entry, "privateIpAddress")?;
            let primary = opt_element(entry, "primary").unwrap_or_default() == "true";
            Some((address, primary))
        })
        .collect();
    NodeInterface {
        network_interface_id: element(set, "networkInterfaceId"),
        subnet_id: element(set, "subnetId"),
        addresses,
    }
}

/// The address a `Node` document reports as its own: the `InternalIP` address of the
/// instance behind it.
fn node_internal_ip(node: &Value) -> String {
    node["status"]["addresses"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|address| address["type"] == json!("InternalIP"))
        .and_then(|address| address["address"].as_str())
        .unwrap_or_else(|| panic!("the node must report its own address, got {node}"))
        .to_string()
}

/// The instance the `Node` document stands for: the id its `spec.providerID` names
/// (`aws:///<zone>/<instance id>`).
fn node_instance_id(node: &Value) -> String {
    node["spec"]["providerID"]
        .as_str()
        .and_then(|provider| provider.rsplit('/').next())
        .filter(|instance_id| !instance_id.is_empty())
        .unwrap_or_else(|| panic!("the node must name its instance, got {node}"))
        .to_string()
}

/// The pods of the `default` namespace whose name begins with `prefix`, paired with
/// the address each reports — a pod that reports none is paired with `None`.
fn pod_addresses(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    what: &str,
    prefix: &str,
) -> BTreeMap<String, Option<String>> {
    let listed = k8s(
        runtime,
        endpoint,
        what,
        "GET",
        "/api/v1/namespaces/default/pods",
        None,
    );
    assert_eq!(
        listed.status, 200,
        "{what}: the pod collection must be served, got {listed:?}"
    );
    listed.body["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|pod| {
            let name = pod["metadata"]["name"].as_str()?.to_string();
            if !name.starts_with(prefix) {
                return None;
            }
            Some((
                name,
                pod["status"]["podIP"].as_str().map(str::to_string),
            ))
        })
        .collect()
}

#[test]
fn a_pods_address_is_a_secondary_private_address_of_its_nodes_interface() {
    std::thread::Builder::new()
        .name("eks-pod-address".to_string())
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
            let cluster = "pod-address-host";
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
                    answer.body["cluster"]["status"] == json!("ACTIVE")
                        && answer.body["cluster"]["endpoint"].is_string()
                },
            );
            let endpoint = active.body["cluster"]["endpoint"]
                .as_str()
                .expect("the published API server address")
                .to_string();
            let nodegroup = eks(
                &router,
                &runtime,
                "POST",
                &format!("/clusters/{cluster}/node-groups"),
                Some(json!({
                    "nodegroupName": NODEGROUP,
                    "nodeRole": role_arn("eksNodeRole"),
                    "subnets": subnets,
                    "scalingConfig": { "minSize": 1, "maxSize": 1, "desiredSize": 1 },
                })),
            );
            assert_eq!(
                nodegroup.status, 200,
                "setup: the node group must be created, got {nodegroup:?} ({})",
                nodegroup.message()
            );
            read_until(
                "the node group to reach ACTIVE",
                || {
                    eks(
                        &router,
                        &runtime,
                        "GET",
                        &format!("/clusters/{cluster}/node-groups/{NODEGROUP}"),
                        None,
                    )
                },
                |answer| answer.body["nodegroup"]["status"] == json!("ACTIVE"),
            );
            // THE NODE the pods below are placed on, and the instance behind it.
            let nodes = api_until(
                &runtime,
                &endpoint,
                "the cluster to report the node its node group launched",
                "/api/v1/nodes",
                |body| {
                    body["items"]
                        .as_array()
                        .is_some_and(|items| items.len() == 1)
                },
            );
            let node = nodes["items"][0].clone();
            let instance_id = node_instance_id(&node);
            let internal_ip = node_internal_ip(&node);

            // WHAT THE INTERFACE HELD BEFORE ANY POD: the node's own address, as its
            // primary one, and nothing else.
            let before = node_interface(&router, &runtime, "before any pod", &instance_id);
            let block = subnet_block(&router, &runtime, &before.subnet_id);
            assert_eq!(
                before.addresses,
                BTreeMap::from([(internal_ip.clone(), true)]),
                "before any pod: the node's interface must hold its own address as its \
                 primary one and nothing else, got {before:?}"
            );

            // The rows differ along how many pods the node already holds: the first
            // pod, and each further one — every one of them addressed out of the same
            // subnet, and no two of them alike.
            let rows: [(&str, &str); 3] = [
                ("the first pod on the node", "addressed-one"),
                ("a second pod on the same node", "addressed-two"),
                ("a third pod on the same node", "addressed-three"),
            ];
            let mut held: Vec<String> = Vec::new();
            for (case, pod) in rows {
                let created = k8s(
                    &runtime,
                    &endpoint,
                    case,
                    "POST",
                    "/api/v1/namespaces/default/pods",
                    Some(&json!({
                        "metadata": { "name": pod },
                        "spec": { "containers": [{ "name": "app", "image": IMAGE }] },
                    })),
                );
                assert_eq!(
                    created.status, 201,
                    "case {case:?}: the pod must be created, got {created:?}"
                );
                let addressed = api_until(
                    &runtime,
                    &endpoint,
                    &format!("case {case:?}: the pod to be addressed"),
                    &format!("/api/v1/namespaces/default/pods/{pod}"),
                    |body| body["status"]["podIP"].is_string(),
                );
                let address = addressed["status"]["podIP"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                // ONE ADDRESS, REPORTED IN BOTH READINGS: "the first entry must match
                // the podIP field" (`PodStatus.podIPs`).
                assert_eq!(
                    addressed["status"]["podIPs"],
                    json!([{ "ip": address }]),
                    "case {case:?}: expected the list form to carry the same address \
                     ({address}), got {}",
                    addressed["status"]
                );
                // IT IS AN ADDRESS OF THE NODE'S SUBNET, and not the node's own.
                assert!(
                    block_contains(&block, &address),
                    "case {case:?}: expected an address inside the subnet block {block}, got \
                     {address:?}"
                );
                assert_ne!(
                    address, internal_ip,
                    "case {case:?}: the pod must not be given the node's own address, got \
                     {address:?}"
                );
                assert!(
                    !held.contains(&address),
                    "case {case:?}: expected an address no other pod holds, got {address:?} \
                     beside {held:?}"
                );
                assert_eq!(
                    addressed["spec"]["nodeName"], json!(node["metadata"]["name"]),
                    "case {case:?}: the pod must be on the node whose interface addressed it, \
                     got {}",
                    addressed["spec"]
                );
                held.push(address.clone());

                // AMAZON EC2 REPORTS IT ON THAT VERY INTERFACE, as a secondary
                // address, beside the node's own primary one.
                let after = node_interface(&router, &runtime, case, &instance_id);
                assert_eq!(
                    after.network_interface_id, before.network_interface_id,
                    "case {case:?}: the node must still be on one interface, got {after:?}"
                );
                let mut expected: BTreeMap<String, bool> =
                    BTreeMap::from([(internal_ip.clone(), true)]);
                expected.extend(held.iter().map(|address| (address.clone(), false)));
                assert_eq!(
                    after.addresses, expected,
                    "case {case:?}: expected the interface to hold every pod address as a \
                     secondary address, got {after:?}"
                );
            }

            // A POD THE CLUSTER COULD PLACE NOWHERE HOLDS NO ADDRESS: it names a node
            // the cluster does not hold, so nothing addressed it and the interface is
            // unchanged.
            let unplaceable = k8s(
                &runtime,
                &endpoint,
                "a pod that names a node the cluster does not hold",
                "POST",
                "/api/v1/namespaces/default/pods",
                Some(&json!({
                    "metadata": { "name": "addressed-nowhere" },
                    "spec": {
                        "nodeName": "ip-no-such-node.compute.internal",
                        "containers": [{ "name": "app", "image": IMAGE }],
                    },
                })),
            );
            assert_eq!(
                unplaceable.status, 201,
                "a pod that names an absent node must still be admitted, got {unplaceable:?}"
            );
            let unplaced = k8s(
                &runtime,
                &endpoint,
                "the pod the cluster could place nowhere",
                "GET",
                "/api/v1/namespaces/default/pods/addressed-nowhere",
                None,
            );
            assert_eq!(
                (
                    unplaced.body["status"]["podIP"].clone(),
                    unplaced.body["status"]["podIPs"].clone(),
                ),
                (Value::Null, Value::Null),
                "a pod no node took must report no address at all, got {}",
                unplaced.body["status"]
            );
            let unchanged = node_interface(
                &router,
                &runtime,
                "after the pod the cluster could place nowhere",
                &instance_id,
            );
            assert_eq!(
                unchanged.addresses.len(),
                held.len() + 1,
                "a pod the cluster placed nowhere must take no address of the node's \
                 interface, got {unchanged:?}"
            );

            // AN ADDRESS IS GIVEN BACK when the pod that holds it is gone: a workload
            // whose replica count shrinks takes one of its pods away with it.
            let workload = json!({
                "metadata": { "name": "addressed-app" },
                "spec": {
                    "replicas": 2,
                    "template": {
                        "metadata": { "labels": { "app": "addressed" } },
                        "spec": { "containers": [{ "name": "app", "image": IMAGE }] },
                    },
                },
            });
            let deployed = k8s(
                &runtime,
                &endpoint,
                "the workload whose pods are addressed",
                "POST",
                "/apis/apps/v1/namespaces/default/deployments",
                Some(&workload),
            );
            assert_eq!(
                deployed.status, 201,
                "the workload must be created, got {deployed:?}"
            );
            let addressed_replicas = loop_until_addressed(&runtime, &endpoint, 2);
            let scaled = k8s(
                &runtime,
                &endpoint,
                "the workload scaled down to one replica",
                "PUT",
                "/apis/apps/v1/namespaces/default/deployments/addressed-app",
                Some(&json!({
                    "metadata": { "name": "addressed-app" },
                    "spec": {
                        "replicas": 1,
                        "template": workload["spec"]["template"].clone(),
                    },
                })),
            );
            assert_eq!(
                scaled.status, 200,
                "the workload must be scaled down, got {scaled:?}"
            );
            let left = loop_until_addressed(&runtime, &endpoint, 1);
            let kept = left
                .values()
                .next()
                .cloned()
                .expect("the replica the workload still runs holds an address");
            let gone: Vec<String> = addressed_replicas
                .iter()
                .filter(|(pod, _)| !left.contains_key(*pod))
                .map(|(_, address)| address.clone())
                .collect();
            assert_eq!(
                gone.len(),
                1,
                "scaling the workload down must take exactly one replica away, got {:?} then \
                 {:?}",
                addressed_replicas,
                left
            );
            let released = node_interface(
                &router,
                &runtime,
                "after the workload was scaled down",
                &instance_id,
            );
            assert!(
                !released.addresses.contains_key(&gone[0]),
                "the address of the replica that is gone must be back in its subnet, got \
                 {released:?} still holding {:?}",
                gone[0]
            );
            assert_eq!(
                released.addresses.get(&kept),
                Some(&false),
                "the replica that is still running must keep its address ({kept}), got \
                 {released:?}"
            );
        })
        .expect("spawn the eks pod-address thread")
        .join()
        .expect("the eks pod-address thread");
}

/// Poll until the workload's replicas are exactly `wanted` and each of them holds an
/// address, and answer them by name.
///
/// A replica is created, placed and addressed by the same passes every other pod of
/// the cluster goes through, so a caller polls for the condition rather than waiting
/// a span.
fn loop_until_addressed(
    runtime: &tokio::runtime::Runtime,
    endpoint: &str,
    wanted: usize,
) -> BTreeMap<String, String> {
    let what = format!("{wanted} addressed replica(s) of the workload");
    for _ in 0..POLLS {
        let held = pod_addresses(runtime, endpoint, &what, "addressed-app-");
        if held.len() == wanted && held.values().all(Option::is_some) {
            return held
                .into_iter()
                .filter_map(|(pod, address)| address.map(|address| (pod, address)))
                .collect();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!(
        "expected {what}; the workload's pods are {:?}",
        pod_addresses(runtime, endpoint, &what, "addressed-app-")
    );
}
