// SPDX-License-Identifier: CC0-1.0

//! The core `v1` SERVICES a cluster's published Kubernetes API server holds.
//!
//! The RULE under test: the address a cluster publishes serves the core `v1`
//! `services` collection — a client creates, reads, lists and deletes a Service
//! there rather than being told the verb is unsupported — and a Service's
//! `clusterIP` is ALLOCATED out of the block the cluster's own record publishes:
//! `KubernetesNetworkConfigResponse$serviceIpv4Cidr` is "the CIDR block that
//! Kubernetes `Pod` and `Service` object IP addresses are assigned from", so the
//! address a Service reports is inside the block `DescribeCluster` reports, is
//! distinct from every other Service's of that cluster, is read back unchanged, and
//! goes back to the block when the Service is deleted.
//!
//! The suites drive that over a TABLE whose rows differ along what the rule
//! generalises over: the `type` of the Service (the default `ClusterIP`, a headless
//! one, `NodePort`, `LoadBalancer` and `ExternalName`), the ports it declares (one
//! unnamed port, several named ones, a `targetPort` named rather than numbered),
//! whether it selects pods at all, and the block the cluster was created with (the
//! one Amazon EKS assigns and one the caller named) — because the rule is about the
//! cluster's own block rather than about any one value.
//!
//! The refusals are driven over a table too, and each row asserts that the refused
//! manifest stored NOTHING: a Service the registry rejected must not be in the
//! collection afterwards.

use ipnet::IpNet;
use k8s_pb::api::core::v1 as core_v1;
use k8s_pb::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_pb::apimachinery::pkg::runtime::{TypeMeta, Unknown};
use k8s_pb::apimachinery::pkg::util::intstr::IntOrString;
use prost::Message as _;
use service_com_amazonaws_eks_awswesleyfrontend::state::MAX_PORTS_PER_SERVICE;
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{
    account_bearer_token, dial, dial_bytes, kubeconfig_of, poll_until_active, ApiAnswer, Kubeconfig,
};
use crate::wire::{call, emulator};

/// The namespace the suites create their Services in, which every cluster has.
const NAMESPACE: &str = "default";

/// The Service every Kubernetes cluster holds for its own API server.
const API_SERVER_SERVICE: &str = "kubernetes";

/// The collection path of a namespace's Services.
fn services_path(namespace: &str) -> String {
    format!("/api/v1/namespaces/{namespace}/services")
}

/// One Service's path.
fn service_path(namespace: &str, name: &str) -> String {
    format!("{}/{name}", services_path(namespace))
}

/// Create a cluster — with the service CIDR `service_cidr` when a case names one —
/// wait for the clock-owed provisioning, and answer the kubeconfig a client builds
/// beside the service block the cluster published.
async fn provisioned_cluster(
    router: &MultiServiceRouter,
    name: &str,
    service_cidr: Option<&str>,
) -> (Kubeconfig, String) {
    let mut body = serde_json::json!({
        "name": name,
        "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
        "resourcesVpcConfig": { "subnetIds": ["subnet-11111111", "subnet-22222222"] },
    });
    if let Some(cidr) = service_cidr {
        body["kubernetesNetworkConfig"] = serde_json::json!({ "serviceIpv4Cidr": cidr });
    }
    let created = call(router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateCluster({name}), got {created:?}"
    );
    let described = poll_until_active(router, REGION, name).await;
    let published = described.cluster()["kubernetesNetworkConfig"]["serviceIpv4Cidr"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup: the cluster {name} publishes a service CIDR, got {described:?}")
        })
        .to_string();
    (kubeconfig_of(name, &described), published)
}

/// The block `cidr` states, read with the same crate the emulator reads a CIDR
/// block with.
fn block(case: &str, cidr: &str) -> IpNet {
    cidr.parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {cidr:?} must be a CIDR block: {error}"))
}

/// The `spec.clusterIP` of one answered Service, asserted to be an address inside
/// the block the cluster published.
fn addressed_in(case: &str, cidr: &str, answered: &ApiAnswer) -> std::net::IpAddr {
    let reported = answered.body["spec"]["clusterIP"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a Service reports a clusterIP, got {answered:?}"));
    let address: std::net::IpAddr = reported.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: clusterIP {reported:?} must be an IP address: {error}")
    });
    assert!(
        block(case, cidr).contains(&address),
        "case {case:?}: the clusterIP must come out of the cluster's own block {cidr}, got \
         {reported}"
    );
    // "ClusterIP and ClusterIPs must agree": the list carries the same address.
    assert_eq!(
        answered.body["spec"]["clusterIPs"],
        serde_json::json!([reported]),
        "case {case:?}: clusterIPs must agree with clusterIP, got {answered:?}"
    );
    address
}

/// One row of the create table: the manifest a client sends and what the stored
/// object must report.
struct ServiceCase {
    label: &'static str,
    name: &'static str,
    manifest: serde_json::Value,
    /// `spec.type` the answer must report — the default for a manifest naming none.
    reported_type: &'static str,
    /// Whether the Service is allocated an address out of the cluster's block.
    addressed: bool,
    /// Whether every port must report a `nodePort` out of the API server's range.
    node_ported: bool,
}

/// The table: one row per `type` the resource serves, with the port and selector
/// shapes a client writes.
fn service_cases() -> Vec<ServiceCase> {
    vec![
        ServiceCase {
            label: "a Service naming no type, with one unnamed port",
            name: "svc-default-type",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-default-type", "labels": { "app": "store" } },
                "spec": {
                    "selector": { "app": "store" },
                    "ports": [{ "port": 80, "targetPort": 8080 }],
                },
            }),
            reported_type: "ClusterIP",
            addressed: true,
            node_ported: false,
        },
        ServiceCase {
            label: "a ClusterIP Service with several named ports and a named targetPort",
            name: "svc-multi-port",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-multi-port" },
                "spec": {
                    "type": "ClusterIP",
                    "selector": { "app": "web" },
                    "ports": [
                        { "name": "http", "port": 80, "targetPort": "http" },
                        { "name": "metrics", "port": 9090, "protocol": "TCP" },
                    ],
                },
            }),
            reported_type: "ClusterIP",
            addressed: true,
            node_ported: false,
        },
        ServiceCase {
            label: "a headless Service, which asks for no address",
            name: "svc-headless",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-headless" },
                "spec": {
                    "clusterIP": "None",
                    "selector": { "app": "stateful" },
                    "ports": [{ "port": 5432 }],
                },
            }),
            reported_type: "ClusterIP",
            addressed: false,
            node_ported: false,
        },
        ServiceCase {
            label: "a NodePort Service, which selects no pods",
            name: "svc-node-port",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-node-port" },
                "spec": {
                    "type": "NodePort",
                    "ports": [{ "port": 8080, "protocol": "UDP" }],
                },
            }),
            reported_type: "NodePort",
            addressed: true,
            node_ported: true,
        },
        ServiceCase {
            label: "a LoadBalancer Service",
            name: "svc-load-balancer",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-load-balancer" },
                "spec": {
                    "type": "LoadBalancer",
                    "selector": { "app": "edge" },
                    "ports": [{ "port": 443, "targetPort": 8443 }],
                },
            }),
            reported_type: "LoadBalancer",
            addressed: true,
            node_ported: true,
        },
        ServiceCase {
            label: "an ExternalName Service, which has no address of its own",
            name: "svc-external-name",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-external-name" },
                "spec": {
                    "type": "ExternalName",
                    "externalName": "database.example.internal",
                },
            }),
            reported_type: "ExternalName",
            addressed: false,
            node_ported: false,
        },
    ]
}

/// RULE: the core group advertises the resource, and every cluster holds the Service
/// its own API server is reached by inside the cluster, on the first address of the
/// block the cluster published.
#[tokio::test]
async fn the_core_group_serves_services_and_holds_the_api_server_service() {
    let router = emulator();
    let (config, cidr) = provisioned_cluster(&router, "svc-discovery", None).await;

    let resources = dial(&config, "the core resource list", "GET", "/api/v1", None).await;
    assert_eq!(
        resources.status, 200,
        "the core resource list must be served, got {resources:?}"
    );
    let row = resources.body["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("expected resources, got {resources:?}"))
        .iter()
        .find(|row| row["name"] == serde_json::json!("services"))
        .cloned()
        .unwrap_or_else(|| panic!("expected the services resource, got {resources:?}"));
    assert_eq!(
        row["kind"],
        serde_json::json!("Service"),
        "the resource carries its kind, got {row:?}"
    );
    assert_eq!(
        row["namespaced"],
        serde_json::json!(true),
        "a Service lives in a namespace, got {row:?}"
    );
    for verb in ["create", "get", "list", "delete"] {
        assert!(
            row["verbs"]
                .as_array()
                .is_some_and(|verbs| verbs.contains(&serde_json::json!(verb))),
            "the resource must serve {verb}, got {row:?}"
        );
    }

    let case = "the API server's own Service";
    let answered = dial(
        &config,
        case,
        "GET",
        &service_path(NAMESPACE, API_SERVER_SERVICE),
        None,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: every cluster holds default/kubernetes, got {answered:?}"
    );
    let address = addressed_in(case, &cidr, &answered);
    let first = block(case, &cidr)
        .hosts()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: {cidr} has addresses"));
    assert_eq!(
        address, first,
        "case {case:?}: the API server's Service is on the first address of {cidr}, got {address}"
    );
    assert_eq!(
        answered.body["spec"]["ports"][0]["port"],
        serde_json::json!(443),
        "case {case:?}: it answers on the API server's port, got {answered:?}"
    );
}

/// RULE: each row is created, answered as the object that was stored, read back
/// through the collection and through its own path, and addressed out of the
/// cluster's block exactly when its type has an address — with every address
/// distinct.
#[tokio::test]
async fn a_service_of_each_type_is_stored_and_addressed_from_the_published_block() {
    let router = emulator();
    let (config, cidr) = provisioned_cluster(&router, "svc-types", None).await;
    let mut addresses: Vec<std::net::IpAddr> = Vec::new();

    for row in service_cases() {
        let case = row.label;
        let created = dial(
            &config,
            case,
            "POST",
            &services_path(NAMESPACE),
            Some(&row.manifest),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: a Service must be created, got {created:?}"
        );
        assert_eq!(
            created.body["kind"],
            serde_json::json!("Service"),
            "case {case:?}: the answer is the Service object, got {created:?}"
        );
        assert_eq!(
            created.body["spec"]["type"],
            serde_json::json!(row.reported_type),
            "case {case:?}: expected type {}, got {created:?}",
            row.reported_type
        );
        if row.addressed {
            addresses.push(addressed_in(case, &cidr, &created));
        } else {
            let reported = created.body["spec"]["clusterIP"].as_str();
            assert!(
                reported.is_none() || reported == Some("None"),
                "case {case:?}: this type has no allocated address, got {created:?}"
            );
        }
        if row.node_ported {
            for port in created.body["spec"]["ports"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected ports, got {created:?}"))
            {
                let node_port = port["nodePort"].as_i64().unwrap_or_else(|| {
                    panic!("case {case:?}: this type allocates a nodePort, got {created:?}")
                });
                assert!(
                    (30000..=32767).contains(&node_port),
                    "case {case:?}: a nodePort comes out of the API server's range, got \
                     {node_port}"
                );
            }
        }

        // The object is READ BACK the way a client reads it: through its own path,
        // and in the collection.
        let read = dial(
            &config,
            case,
            "GET",
            &service_path(NAMESPACE, row.name),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored Service must be readable, got {read:?}"
        );
        assert_eq!(
            read.body, created.body,
            "case {case:?}: a read reports what the create stored, got {read:?}"
        );
        let listed = dial(&config, case, "GET", &services_path(NAMESPACE), None).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: the collection must be listable, got {listed:?}"
        );
        assert!(
            listed.body["items"]
                .as_array()
                .is_some_and(|items| items.contains(&created.body)),
            "case {case:?}: the collection must carry the stored Service, got {listed:?}"
        );
    }

    let mut distinct = addresses.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        addresses.len(),
        "two Services of one cluster cannot share an address, got {addresses:?}"
    );

    // A second create of a name the namespace already holds is the API server's
    // conflict, and the address the first one holds is untouched.
    let case = "a Service that already exists";
    let again = dial(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        Some(&service_cases()[0].manifest),
    )
    .await;
    assert_eq!(
        again.status, 409,
        "case {case:?}: expected the already-exists Status, got {again:?}"
    );
}

/// RULE: the block is the CLUSTER's, not a constant — a cluster created with a
/// service CIDR of its own addresses its Services out of THAT block, and two
/// clusters of one emulator address theirs out of their own.
#[tokio::test]
async fn the_addresses_come_out_of_each_cluster_s_own_block() {
    let router = emulator();
    // The two blocks Amazon EKS assigns when a caller names none are
    // `172.20.0.0/16` and `10.100.0.0/16` (`serviceIpv4Cidr`), so a caller-named
    // block is taken from the private space outside the one the service assigns.
    let (named, named_cidr) =
        provisioned_cluster(&router, "svc-named-block", Some("10.96.0.0/12")).await;
    let (assigned, assigned_cidr) = provisioned_cluster(&router, "svc-assigned-block", None).await;
    assert_eq!(
        named_cidr, "10.96.0.0/12",
        "the cluster must publish the block its create named, got {named_cidr}"
    );
    assert_ne!(
        named_cidr, assigned_cidr,
        "the two clusters must publish different blocks, got {named_cidr} and {assigned_cidr}"
    );

    let manifest = serde_json::json!({
        "metadata": { "name": "svc-blocked" },
        "spec": { "selector": { "app": "any" }, "ports": [{ "port": 80 }] },
    });
    for (case, config, cidr) in [
        ("the cluster whose create named a block", &named, &named_cidr),
        ("the cluster the service assigned a block", &assigned, &assigned_cidr),
    ] {
        let created = dial(
            config,
            case,
            "POST",
            &services_path(NAMESPACE),
            Some(&manifest),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: a Service must be created, got {created:?}"
        );
        addressed_in(case, cidr, &created);
    }
}

/// RULE: deleting a Service gives up the object AND the address it held, which the
/// next allocation may hand out again.
#[tokio::test]
async fn a_deleted_service_gives_up_its_object_and_its_address() {
    let router = emulator();
    let (config, cidr) = provisioned_cluster(&router, "svc-deletion", None).await;
    let of = |name: &str| {
        serde_json::json!({
            "metadata": { "name": name },
            "spec": { "selector": { "app": name }, "ports": [{ "port": 80 }] },
        })
    };

    let case = "the first Service";
    let first = dial(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        Some(&of("svc-first")),
    )
    .await;
    assert_eq!(
        first.status, 201,
        "case {case:?}: a Service must be created, got {first:?}"
    );
    let released = addressed_in(case, &cidr, &first);

    let case = "the second Service";
    let second = dial(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        Some(&of("svc-second")),
    )
    .await;
    assert_eq!(
        second.status, 201,
        "case {case:?}: a Service must be created, got {second:?}"
    );
    let held = addressed_in(case, &cidr, &second);
    assert_ne!(
        released, held,
        "case {case:?}: the two Services must hold different addresses, got {held}"
    );

    let case = "deleting the first Service";
    let deleted = dial(
        &config,
        case,
        "DELETE",
        &service_path(NAMESPACE, "svc-first"),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: a delete must be served, got {deleted:?}"
    );
    assert_eq!(
        deleted.body["status"],
        serde_json::json!("Success"),
        "case {case:?}: a delete is answered with the Success Status, got {deleted:?}"
    );
    let gone = dial(
        &config,
        case,
        "GET",
        &service_path(NAMESPACE, "svc-first"),
        None,
    )
    .await;
    assert_eq!(
        gone.status, 404,
        "case {case:?}: the deleted Service must be gone, got {gone:?}"
    );
    let gone_again = dial(
        &config,
        case,
        "DELETE",
        &service_path(NAMESPACE, "svc-first"),
        None,
    )
    .await;
    assert_eq!(
        gone_again.status, 404,
        "case {case:?}: deleting it again must be the not-found Status, got {gone_again:?}"
    );

    // The address the delete gave back is the lowest free one, so the next Service
    // is allocated it again rather than being refused or given a fresh address.
    let case = "the Service created after the delete";
    let third = dial(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        Some(&of("svc-third")),
    )
    .await;
    assert_eq!(
        third.status, 201,
        "case {case:?}: a Service must be created, got {third:?}"
    );
    assert_eq!(
        addressed_in(case, &cidr, &third),
        released,
        "case {case:?}: the released address must be allocatable again, got {third:?}"
    );
}

/// One row of the refusal table: a manifest the Service registry does not store,
/// and the status and sentence it is refused with.
struct RefusalCase {
    label: &'static str,
    manifest: serde_json::Value,
    status: u16,
    /// What the refusal's own sentence has to carry.
    message: String,
}

/// RULE: a manifest the registry refuses stores nothing — the refusal names what it
/// could not accept, and the collection is unchanged afterwards.
#[tokio::test]
async fn a_refused_manifest_stores_no_service() {
    let router = emulator();
    let (config, cidr) = provisioned_cluster(&router, "svc-refusals", None).await;

    // The address an existing Service holds, so one row can ask for an address that
    // is already allocated.
    let taken = dial(
        &config,
        "the Service holding an address",
        "POST",
        &services_path(NAMESPACE),
        Some(&serde_json::json!({
            "metadata": { "name": "svc-holder" },
            "spec": { "selector": { "app": "holder" }, "ports": [{ "port": 80 }] },
        })),
    )
    .await;
    assert_eq!(
        taken.status, 201,
        "setup: a Service must be created, got {taken:?}"
    );
    let allocated = addressed_in("the Service holding an address", &cidr, &taken).to_string();

    let rows = vec![
        RefusalCase {
            label: "a type outside the four the resource serves",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-bad-type" },
                "spec": { "type": "Ambassador", "ports": [{ "port": 80 }] },
            }),
            status: 422,
            message: "spec.type: Unsupported value: \"Ambassador\"".to_string(),
        },
        RefusalCase {
            label: "a Service declaring no port at all",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-no-ports" },
                "spec": { "selector": { "app": "none" } },
            }),
            status: 422,
            message: "spec.ports: Required value".to_string(),
        },
        RefusalCase {
            label: "a port number outside the port range",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-bad-port" },
                "spec": { "ports": [{ "port": 70000 }] },
            }),
            status: 422,
            message: "spec.ports[0].port: Invalid value: must be between 1 and 65535, inclusive"
                .to_string(),
        },
        RefusalCase {
            label: "a multi-port Service whose second port is unnamed",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-unnamed-port" },
                "spec": { "ports": [{ "name": "http", "port": 80 }, { "port": 443 }] },
            }),
            status: 422,
            message: "spec.ports[1].name: Required value".to_string(),
        },
        RefusalCase {
            label: "a protocol no Service port runs",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-bad-protocol" },
                "spec": { "ports": [{ "port": 80, "protocol": "QUIC" }] },
            }),
            status: 422,
            message: "spec.ports[0].protocol: Unsupported value: \"QUIC\"".to_string(),
        },
        RefusalCase {
            label: "a Service declaring more ports than one may hold",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-too-many-ports" },
                "spec": {
                    "ports": (0..=MAX_PORTS_PER_SERVICE)
                        .map(|index| serde_json::json!({
                            "name": format!("port-{index}"),
                            "port": 1024 + index,
                        }))
                        .collect::<Vec<serde_json::Value>>(),
                },
            }),
            status: 422,
            message: format!(
                "spec.ports: Too many: {}: must have at most {MAX_PORTS_PER_SERVICE} items",
                MAX_PORTS_PER_SERVICE + 1
            ),
        },
        RefusalCase {
            label: "an ExternalName Service naming no external name",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-no-external-name" },
                "spec": { "type": "ExternalName" },
            }),
            status: 422,
            message: "spec.externalName: Required value".to_string(),
        },
        RefusalCase {
            label: "an address outside the cluster's own block",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-foreign-address" },
                "spec": { "clusterIP": "192.0.2.9", "ports": [{ "port": 80 }] },
            }),
            status: 422,
            message: format!(
                "failed to allocate IP 192.0.2.9: the provided IP (192.0.2.9) is not in the \
                 valid range. The range of valid IPs is {cidr}"
            ),
        },
        RefusalCase {
            label: "an address another Service of the cluster already holds",
            manifest: serde_json::json!({
                "metadata": { "name": "svc-taken-address" },
                "spec": { "clusterIP": allocated, "ports": [{ "port": 80 }] },
            }),
            status: 422,
            message: format!("failed to allocate IP {allocated}: provided IP is already allocated"),
        },
    ];

    for row in rows {
        let case = row.label;
        let refused = dial(
            &config,
            case,
            "POST",
            &services_path(NAMESPACE),
            Some(&row.manifest),
        )
        .await;
        assert_eq!(
            refused.status, row.status,
            "case {case:?}: expected {}, got {refused:?}",
            row.status
        );
        let message = refused.body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&row.message),
            "case {case:?}: the refusal must carry {:?}, got {message:?}",
            row.message
        );
        let name = row.manifest["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = dial(&config, case, "GET", &service_path(NAMESPACE, &name), None).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused manifest stores nothing, got {read:?}"
        );
    }

    // An address the cluster's block DOES hold, and nothing holds yet, is allocated
    // as asked for — the same path, which is what makes the refusals above about the
    // block rather than about naming an address at all.
    let case = "an address inside the cluster's own block";
    let free = block(case, &cidr)
        .hosts()
        .nth(200)
        .unwrap_or_else(|| panic!("case {case:?}: {cidr} has addresses"))
        .to_string();
    let created = dial(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        Some(&serde_json::json!({
            "metadata": { "name": "svc-asked-address" },
            "spec": { "clusterIP": free, "ports": [{ "port": 80 }] },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: the address asked for must be allocated, got {created:?}"
    );
    assert_eq!(
        created.body["spec"]["clusterIP"],
        serde_json::json!(free),
        "case {case:?}: expected the address asked for, got {created:?}"
    );
}

/// The media type a client states when it wrote the body in the Kubernetes
/// protobuf encoding.
const PROTOBUF: &str = "application/vnd.kubernetes.protobuf";

/// RULE: the collection reads a Service in every encoding this API server reads a
/// write body in — the object a client sent as the bytes of its own generated
/// message is the object that is stored, addressed out of the cluster's block like
/// any other.
#[tokio::test]
async fn a_service_sent_in_the_protobuf_encoding_is_stored() {
    let router = emulator();
    let (config, cidr) = provisioned_cluster(&router, "svc-protobuf", None).await;

    // The body a client writing that encoding sends: the four magic bytes, then the
    // envelope naming the kind and carrying the object's own encoded message.
    let service = core_v1::Service {
        metadata: Some(ObjectMeta {
            name: Some("svc-encoded".to_string()),
            ..Default::default()
        }),
        spec: Some(core_v1::ServiceSpec {
            r#type: Some("ClusterIP".to_string()),
            selector: [("app".to_string(), "encoded".to_string())]
                .into_iter()
                .collect(),
            ports: vec![core_v1::ServicePort {
                name: Some("http".to_string()),
                protocol: Some("TCP".to_string()),
                port: Some(80),
                // The one member written either as a number or as the NAME of a
                // container port.
                target_port: Some(IntOrString {
                    str_val: Some("http".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let envelope = Unknown {
        type_meta: Some(TypeMeta {
            api_version: Some("v1".to_string()),
            kind: Some("Service".to_string()),
        }),
        raw: Some(service.encode_to_vec()),
        ..Default::default()
    };
    let mut payload = b"k8s\x00".to_vec();
    envelope
        .encode(&mut payload)
        .expect("setup: the envelope encodes into a growable buffer");

    let case = "a Service sent in the protobuf encoding";
    let created = dial_bytes(
        &config,
        case,
        "POST",
        &services_path(NAMESPACE),
        PROTOBUF,
        Some(&account_bearer_token()),
        &payload,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: the collection must read that encoding, got {created:?}"
    );
    addressed_in(case, &cidr, &created);
    assert_eq!(
        created.body["spec"]["ports"][0]["targetPort"],
        serde_json::json!("http"),
        "case {case:?}: a named targetPort is kept as the message carried it, got {created:?}"
    );
    assert_eq!(
        created.body["spec"]["selector"],
        serde_json::json!({ "app": "encoded" }),
        "case {case:?}: the selector is the message's, got {created:?}"
    );
    let read = dial(
        &config,
        case,
        "GET",
        &service_path(NAMESPACE, "svc-encoded"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the stored Service must be readable, got {read:?}"
    );
    assert_eq!(
        read.body, created.body,
        "case {case:?}: a create in that encoding stores the object, got {read:?}"
    );
}
