// SPDX-License-Identifier: CC0-1.0

//! The `apps/v1` WORKLOADS a cluster's Kubernetes API server serves: the
//! Deployments and DaemonSets a client creates there, and the ones Amazon EKS
//! installs with an add-on.
//!
//! The RULE under test: the API server a cluster publishes serves the `apps/v1`
//! group — its discovery documents and its collections — and a workload created
//! there is BACKED by the pods it runs: a Deployment runs the replicas it asked
//! for, a DaemonSet runs one pod per node the cluster holds, each pod is a pod of
//! the namespace scheduled onto a node of that cluster, and what the workload
//! reports in its `status` is counted off those pods. An EKS-managed add-on is such
//! a workload too: `DescribeAddon` reporting one installed is the claim that its
//! Deployment or DaemonSet runs in the cluster, at the version the add-on was
//! pinned to, so a client finds it there — and removing the add-on takes it away.
//!
//! The suites drive that over a TABLE whose rows differ along what the rule
//! generalises over: the resource (a Deployment and a DaemonSet), the replica count
//! (none named, one, several), how many nodes the cluster holds (and what happens
//! when another joins), the namespace, and — for the managed workloads — which
//! add-on is installed and which resource it installs.
//!
//! The client is the raw, kubeconfig-built one `kubernetes_api.rs` dials with.

use std::time::{Duration, Instant};

use service_com_amazonaws_eks_awswesleyfrontend::state::MAX_KUBERNETES_NAME_LEN;
use smithy_server_runtime::MultiServiceRouter;

use crate::addon_lifecycle::{create_addon, describe_addon};
use crate::kubernetes_api::{dial, ApiAnswer, Kubeconfig};
use crate::kubernetes_cluster_resources::{
    active_nodegroup, instance, published_cluster, register_nodes,
};
use crate::wire::{call, encode_label};

/// How long a polled condition is given before the case fails. Generous: the spans
/// these cases wait out are the service's own, and nothing here measures elapsed
/// time.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a polled condition is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The namespace the EKS-managed add-ons are installed into.
const KUBE_SYSTEM: &str = "kube-system";

/// The path of a namespaced workload collection.
fn workloads_path(namespace: &str, resource: &str) -> String {
    format!("/apis/apps/v1/namespaces/{namespace}/{resource}")
}

/// A Deployment manifest: `replicas` when the row names one, and one container.
fn deployment(name: &str, replicas: Option<u64>) -> serde_json::Value {
    let mut manifest = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name, "labels": { "app": name } },
        "spec": {
            "selector": { "matchLabels": { "app": name } },
            "template": {
                "metadata": { "labels": { "app": name } },
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "000000000000.dkr.ecr.us-west-2.amazonaws.com/app:1.4",
                    }],
                },
            },
        },
    });
    if let Some(replicas) = replicas {
        manifest["spec"]["replicas"] = serde_json::json!(replicas);
    }
    manifest
}

/// A DaemonSet manifest: one container, to run on every node.
fn daemon_set(name: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "DaemonSet",
        "metadata": { "name": name },
        "spec": {
            "selector": { "matchLabels": { "app": name } },
            "template": {
                "metadata": { "labels": { "app": name } },
                "spec": {
                    "containers": [{
                        "name": "agent",
                        "image": "000000000000.dkr.ecr.us-west-2.amazonaws.com/agent:2.0",
                    }],
                },
            },
        },
    })
}

/// The pods of `namespace`, as the client reads them.
async fn pods_of(config: &Kubeconfig, case: &str, namespace: &str) -> Vec<serde_json::Value> {
    let listed = dial(
        config,
        case,
        "GET",
        &format!("/api/v1/namespaces/{namespace}/pods"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: the namespace's pods must be served, got {listed:?}"
    );
    listed.body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a pod list, got {listed:?}"))
        .clone()
}

/// The pods of `namespace` whose name is one of `<workload>-...`: the pods the
/// workload runs.
fn pods_of_workload(pods: &[serde_json::Value], workload: &str) -> Vec<serde_json::Value> {
    pods.iter()
        .filter(|pod| {
            pod["metadata"]["name"]
                .as_str()
                .is_some_and(|name| name.starts_with(&format!("{workload}-")))
        })
        .cloned()
        .collect()
}

/// RULE: the group is served — advertised in the group list, with its own group
/// document and a resource list naming both workload resources.
#[tokio::test]
async fn the_apps_group_is_advertised_and_lists_its_resources() {
    let (_router, _engine, config) = published_cluster("apps-discovery").await;

    let groups = dial(&config, "the group list", "GET", "/apis", None).await;
    assert_eq!(
        groups.status, 200,
        "the group list must be served, got {groups:?}"
    );
    assert!(
        groups.body["groups"]
            .as_array()
            .unwrap_or_else(|| panic!("the group list carries groups, got {groups:?}"))
            .iter()
            .any(|group| group["name"] == serde_json::json!("apps")),
        "the group list must advertise apps, got {groups:?}"
    );

    let group = dial(&config, "the apps group", "GET", "/apis/apps", None).await;
    assert_eq!(
        group.status, 200,
        "the apps group document must be served, got {group:?}"
    );
    assert_eq!(
        group.body["preferredVersion"]["groupVersion"],
        serde_json::json!("apps/v1"),
        "the group's preferred version is apps/v1, got {group:?}"
    );

    let resources = dial(&config, "the apps resources", "GET", "/apis/apps/v1", None).await;
    assert_eq!(
        resources.status, 200,
        "the group's resource list must be served, got {resources:?}"
    );
    // (the resource a client addresses, the kind it carries, the short name) — one
    // row per resource this server serves in the group.
    for (name, kind, short) in [
        ("deployments", "Deployment", "deploy"),
        ("daemonsets", "DaemonSet", "ds"),
    ] {
        let row = resources.body["resources"]
            .as_array()
            .unwrap_or_else(|| panic!("expected resources, got {resources:?}"))
            .iter()
            .find(|row| row["name"] == serde_json::json!(name))
            .cloned()
            .unwrap_or_else(|| panic!("case {name:?}: the list must name it, got {resources:?}"));
        assert_eq!(
            row["kind"],
            serde_json::json!(kind),
            "case {name:?}: expected the kind {kind:?}, got {row:?}"
        );
        assert_eq!(
            row["namespaced"],
            serde_json::json!(true),
            "case {name:?}: a workload lives in a namespace, got {row:?}"
        );
        assert_eq!(
            row["shortNames"],
            serde_json::json!([short]),
            "case {name:?}: expected the short name {short:?}, got {row:?}"
        );
        for verb in ["create", "get", "list"] {
            assert!(
                row["verbs"]
                    .as_array()
                    .is_some_and(|verbs| verbs.contains(&serde_json::json!(verb))),
                "case {name:?}: expected the verb {verb:?}, got {row:?}"
            );
        }
    }
}

/// RULE: a created Deployment is stored as submitted and RUNS the replicas it asked
/// for — real pods of the namespace, scheduled onto the cluster's nodes.
#[tokio::test]
async fn a_created_deployment_runs_the_replicas_it_asked_for() {
    let cluster = "apps-deployments";
    let (router, engine, config) = published_cluster(cluster).await;
    active_nodegroup(&router, cluster, "workers").await;
    register_nodes(
        &engine,
        cluster,
        "workers",
        &[
            instance("i-0aaa1111aaa1111aa", "us-west-2a", "t3.medium", "10.0.1.11"),
            instance("i-0bbb2222bbb2222bb", "us-west-2b", "m5.large", "10.0.2.22"),
        ],
    );

    // (the case, the replica count the manifest names, the count it therefore runs)
    // — a Deployment that names none runs the one Kubernetes defaults to.
    let rows: [(&str, Option<u64>, usize); 4] = [
        ("no replica count at all", None, 1),
        ("one replica", Some(1), 1),
        ("several replicas", Some(3), 3),
        ("more replicas than the cluster has nodes", Some(5), 5),
    ];
    for (index, (case, asked, expected)) in rows.into_iter().enumerate() {
        let name = format!("web-{index}");
        let created = dial(
            &config,
            case,
            "POST",
            &workloads_path("default", "deployments"),
            Some(&deployment(&name, asked)),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected 201 Created, got {created:?}"
        );
        assert_eq!(
            created.body["kind"],
            serde_json::json!("Deployment"),
            "case {case:?}: the answer is the stored Deployment, got {created:?}"
        );
        assert_eq!(
            created.body["apiVersion"],
            serde_json::json!("apps/v1"),
            "case {case:?}: the object carries its group version, got {created:?}"
        );
        assert_eq!(
            created.body["metadata"]["name"],
            serde_json::json!(name),
            "case {case:?}: the object carries the name it was created with, got {created:?}"
        );
        assert_eq!(
            created.body["spec"]["replicas"],
            serde_json::json!(expected),
            "case {case:?}: expected {expected} replicas in the stored spec, got {created:?}"
        );
        // The template is stored as submitted, which is what a client reads back to
        // see what the cluster admitted.
        assert_eq!(
            created.body["spec"]["template"]["spec"]["containers"][0]["image"],
            serde_json::json!("000000000000.dkr.ecr.us-west-2.amazonaws.com/app:1.4"),
            "case {case:?}: the container image is read back, got {created:?}"
        );
        assert_eq!(
            created.body["status"]["replicas"],
            serde_json::json!(expected),
            "case {case:?}: the status counts the pods it runs, got {created:?}"
        );

        // The pods are REAL pods of the namespace: one per replica, each carrying
        // the template's labels and the revision label of its template.
        let pods = pods_of(&config, case, "default").await;
        let own = pods_of_workload(&pods, &name);
        assert_eq!(
            own.len(),
            expected,
            "case {case:?}: expected {expected} pods of {name:?}, got {:?}",
            own.iter()
                .map(|pod| pod["metadata"]["name"].clone())
                .collect::<Vec<serde_json::Value>>()
        );
        for pod in &own {
            assert_eq!(
                pod["metadata"]["labels"]["app"],
                serde_json::json!(name),
                "case {case:?}: a replica carries the template's labels, got {pod:?}"
            );
            assert!(
                pod["metadata"]["labels"]["pod-template-hash"].is_string(),
                "case {case:?}: a replica carries the revision it is of, got {pod:?}"
            );
            assert_eq!(
                pod["spec"]["containers"][0]["image"],
                serde_json::json!("000000000000.dkr.ecr.us-west-2.amazonaws.com/app:1.4"),
                "case {case:?}: a replica runs the template's container, got {pod:?}"
            );
            assert!(
                pod["spec"]["nodeName"].is_string(),
                "case {case:?}: a replica is scheduled onto a node of the cluster, got {pod:?}"
            );
        }

        // And the workload reads back on its own path and in the collection.
        let read = dial(
            &config,
            case,
            "GET",
            &format!("{}/{name}", workloads_path("default", "deployments")),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the Deployment must be readable by name, got {read:?}"
        );
        assert_eq!(
            read.body, created.body,
            "case {case:?}: a read reports the object the create stored, got {read:?}"
        );
        let listed = dial(
            &config,
            case,
            "GET",
            &workloads_path("default", "deployments"),
            None,
        )
        .await;
        assert_eq!(
            listed.body["kind"],
            serde_json::json!("DeploymentList"),
            "case {case:?}: a listing is a DeploymentList, got {listed:?}"
        );
        assert!(
            listed.body["items"]
                .as_array()
                .is_some_and(|items| items
                    .iter()
                    .any(|item| item["metadata"]["name"] == serde_json::json!(name))),
            "case {case:?}: the collection carries the Deployment, got {listed:?}"
        );
    }
}

/// RULE: a DaemonSet runs one pod per node the cluster holds — and converges when
/// another node joins.
#[tokio::test]
async fn a_daemon_set_runs_one_pod_per_node() {
    let cluster = "apps-daemonsets";
    let (router, engine, config) = published_cluster(cluster).await;
    let name = "agent";

    // A cluster with no node runs no copy of the daemon, which is what a client
    // reads rather than a pod no node holds.
    let created = dial(
        &config,
        "a cluster with no node",
        "POST",
        &workloads_path("default", "daemonsets"),
        Some(&daemon_set(name)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case a cluster with no node: expected 201 Created, got {created:?}"
    );
    assert_eq!(
        created.body["status"]["desiredNumberScheduled"],
        serde_json::json!(0),
        "case a cluster with no node: nothing is scheduled, got {created:?}"
    );

    // (the case, the instances the node group holds) — the copies follow the nodes.
    let first = [instance(
        "i-0aaa1111aaa1111aa",
        "us-west-2a",
        "t3.medium",
        "10.0.1.11",
    )];
    let second = [
        instance("i-0aaa1111aaa1111aa", "us-west-2a", "t3.medium", "10.0.1.11"),
        instance("i-0bbb2222bbb2222bb", "us-west-2b", "m5.large", "10.0.2.22"),
    ];
    active_nodegroup(&router, cluster, "workers").await;
    for (case, nodes) in [
        ("one node", first.as_slice()),
        ("another node joining", second.as_slice()),
    ] {
        register_nodes(&engine, cluster, "workers", nodes);
        let read = dial(
            &config,
            case,
            "GET",
            &format!("{}/{name}", workloads_path("default", "daemonsets")),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the DaemonSet must be readable, got {read:?}"
        );
        assert_eq!(
            read.body["status"]["desiredNumberScheduled"],
            serde_json::json!(nodes.len()),
            "case {case:?}: one copy per node is wanted, got {read:?}"
        );
        assert_eq!(
            read.body["status"]["currentNumberScheduled"],
            serde_json::json!(nodes.len()),
            "case {case:?}: one copy per node is running, got {read:?}"
        );

        // Each copy is a pod on a node of its own, which is what a daemon set is.
        let pods = pods_of(&config, case, "default").await;
        let own = pods_of_workload(&pods, name);
        assert_eq!(
            own.len(),
            nodes.len(),
            "case {case:?}: expected one pod per node, got {own:?}"
        );
        let mut placed: Vec<String> = own
            .iter()
            .filter_map(|pod| pod["spec"]["nodeName"].as_str().map(str::to_string))
            .collect();
        placed.sort();
        placed.dedup();
        assert_eq!(
            placed.len(),
            nodes.len(),
            "case {case:?}: each copy runs on a node of its own, got {placed:?}"
        );
        for expected in nodes {
            let node_name = expected.node_name();
            assert!(
                placed.iter().any(|node| node == node_name),
                "case {case:?}: the node {node_name:?} must run a copy, got {placed:?}"
            );
        }
    }
}

/// RULE: a workload is changed and removed through the same collection: a replaced
/// spec converges the pods it runs, and a delete takes the workload and its pods
/// away.
#[tokio::test]
async fn a_workload_is_scaled_and_deleted_through_its_own_path() {
    let cluster = "apps-lifecycle";
    let (router, engine, config) = published_cluster(cluster).await;
    active_nodegroup(&router, cluster, "workers").await;
    register_nodes(
        &engine,
        cluster,
        "workers",
        &[instance(
            "i-0aaa1111aaa1111aa",
            "us-west-2a",
            "t3.medium",
            "10.0.1.11",
        )],
    );
    let name = "scaled";
    let path = format!("{}/{name}", workloads_path("default", "deployments"));
    let created = dial(
        &config,
        "the workload to scale",
        "POST",
        &workloads_path("default", "deployments"),
        Some(&deployment(name, Some(1))),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected 201 Created, got {created:?}"
    );

    // (the case, the replica count the replacement asks for) — scaled up and back
    // down again, so the convergence is shown in both directions.
    for (case, replicas) in [("scaled up", 4_u64), ("scaled back down", 2)] {
        let replaced = dial(
            &config,
            case,
            "PUT",
            &path,
            Some(&deployment(name, Some(replicas))),
        )
        .await;
        assert_eq!(
            replaced.status, 200,
            "case {case:?}: expected 200 from the replace, got {replaced:?}"
        );
        assert_eq!(
            replaced.body["spec"]["replicas"],
            serde_json::json!(replicas),
            "case {case:?}: the stored spec is the one submitted, got {replaced:?}"
        );
        assert_eq!(
            replaced.body["status"]["replicas"],
            serde_json::json!(replicas),
            "case {case:?}: the pods it runs converge to the count, got {replaced:?}"
        );
        let pods = pods_of(&config, case, "default").await;
        assert_eq!(
            pods_of_workload(&pods, name).len(),
            usize::try_from(replicas).unwrap_or_default(),
            "case {case:?}: the namespace holds one pod per replica, got {pods:?}"
        );
    }

    let deleted = dial(&config, "the delete", "DELETE", &path, None).await;
    assert_eq!(
        deleted.status, 200,
        "case the delete: expected 200, got {deleted:?}"
    );
    let gone = dial(&config, "the read after the delete", "GET", &path, None).await;
    assert_eq!(
        gone.status, 404,
        "case the read after the delete: the workload is gone, got {gone:?}"
    );
    let pods = pods_of(&config, "the pods after the delete", "default").await;
    assert!(
        pods_of_workload(&pods, name).is_empty(),
        "case the pods after the delete: a deleted workload runs no pod, got {pods:?}"
    );
}

/// RULE: a workload manifest the API server cannot admit is refused with the
/// invalid `Status`, and nothing is stored.
#[tokio::test]
async fn a_workload_the_server_cannot_admit_is_refused() {
    let (_router, _engine, config) = published_cluster("apps-refusals").await;

    // (the case, the manifest, the status the API server answers it with) — the
    // rows differ along WHICH part of the manifest cannot be admitted.
    let rows: Vec<(&str, serde_json::Value, u16)> = vec![
        (
            "a manifest naming no name",
            serde_json::json!({
                "spec": {
                    "template": { "spec": { "containers": [{ "name": "app", "image": "a:1" }] } },
                },
            }),
            422,
        ),
        (
            "a manifest carrying no pod template",
            serde_json::json!({ "metadata": { "name": "no-template" }, "spec": {} }),
            422,
        ),
        (
            "a template whose pod has no container",
            serde_json::json!({
                "metadata": { "name": "no-containers" },
                "spec": { "template": { "spec": { "containers": [] } } },
            }),
            422,
        ),
        (
            "a name no Kubernetes object may carry",
            serde_json::json!({
                "metadata": { "name": "A".repeat(MAX_KUBERNETES_NAME_LEN + 1) },
                "spec": {
                    "template": { "spec": { "containers": [{ "name": "app", "image": "a:1" }] } },
                },
            }),
            422,
        ),
        (
            "a template whose metadata is not a map",
            serde_json::json!({
                "metadata": { "name": "mistyped-metadata" },
                "spec": {
                    "template": {
                        "metadata": "not-a-map",
                        "spec": { "containers": [{ "name": "app", "image": "a:1" }] },
                    },
                },
            }),
            422,
        ),
        (
            "a namespace the cluster does not hold",
            serde_json::json!({
                "metadata": { "name": "nowhere" },
                "spec": {
                    "template": { "spec": { "containers": [{ "name": "app", "image": "a:1" }] } },
                },
            }),
            404,
        ),
    ];
    for (case, manifest, expected) in rows {
        let namespace = if case == "a namespace the cluster does not hold" {
            "no-such-namespace"
        } else {
            "default"
        };
        let refused = dial(
            &config,
            case,
            "POST",
            &workloads_path(namespace, "deployments"),
            Some(&manifest),
        )
        .await;
        assert_eq!(
            refused.status, expected,
            "case {case:?}: expected {expected}, got {refused:?}"
        );
        assert_eq!(
            refused.body["kind"],
            serde_json::json!("Status"),
            "case {case:?}: a refusal is a Status object, got {refused:?}"
        );
    }
}

/// `DescribeAddon` until the add-on reports a status in which its resources have
/// been applied, or the deadline passes.
async fn poll_until_installed(
    router: &MultiServiceRouter,
    cluster: &str,
    addon: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = describe_addon(router, cluster, addon).await;
        let status = described.body["addon"]["status"].clone();
        if status == serde_json::json!("ACTIVE") || status == serde_json::json!("DEGRADED") {
            return described.body["addon"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "add-on {addon:?}: the clock owes it an installed status, got {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: an installed EKS-managed add-on runs its own workload in the cluster, at
/// the version the add-on reports — and removing the add-on takes it away.
#[tokio::test]
async fn an_installed_addon_runs_its_workload_at_the_version_it_reports() {
    let cluster = "apps-addons";
    let (router, engine, config) = published_cluster(cluster).await;
    active_nodegroup(&router, cluster, "workers").await;
    register_nodes(
        &engine,
        cluster,
        "workers",
        &[instance(
            "i-0aaa1111aaa1111aa",
            "us-west-2a",
            "t3.medium",
            "10.0.1.11",
        )],
    );

    // (the add-on, the resource it installs, the workload's name) — the rows differ
    // along which resource the add-on's workload is, which is what each add-on
    // publishes.
    let rows = [
        ("kube-proxy", "daemonsets", "kube-proxy"),
        ("coredns", "deployments", "coredns"),
    ];
    for (addon, resource, workload) in rows {
        let created = create_addon(
            &router,
            cluster,
            &serde_json::json!({ "addonName": addon }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {addon:?}: expected 200 from CreateAddon, got {created:?}"
        );
        let installed = poll_until_installed(&router, cluster, addon).await;
        let version = installed["addonVersion"]
            .as_str()
            .unwrap_or_else(|| panic!("case {addon:?}: expected an addonVersion, got {installed:?}"))
            .to_string();

        let path = format!("{}/{workload}", workloads_path(KUBE_SYSTEM, resource));
        let read = dial(&config, addon, "GET", &path, None).await;
        assert_eq!(
            read.status, 200,
            "case {addon:?}: the add-on's workload must be readable, got {read:?}"
        );
        // The IMAGE the workload runs is the add-on's own, tagged with the version
        // the add-on reports: a client that verifies a pinned add-on reads it here.
        let image = read.body["spec"]["template"]["spec"]["containers"][0]["image"]
            .as_str()
            .unwrap_or_else(|| panic!("case {addon:?}: expected a container image, got {read:?}"))
            .to_string();
        assert!(
            image.ends_with(&format!(":{version}")),
            "case {addon:?}: the image must run the version the add-on reports ({version}), got \
             {image:?}"
        );
        assert!(
            image.contains("/eks/"),
            "case {addon:?}: the image comes out of the Amazon EKS add-on registry, got {image:?}"
        );

        // And it is BACKED by pods of kube-system, like any other workload.
        let pods = pods_of(&config, addon, KUBE_SYSTEM).await;
        let own = pods_of_workload(&pods, workload);
        assert!(
            !own.is_empty(),
            "case {addon:?}: the add-on's workload runs pods, got {pods:?}"
        );
        for pod in &own {
            assert_eq!(
                pod["spec"]["containers"][0]["image"],
                serde_json::json!(image),
                "case {addon:?}: a pod of the workload runs the add-on's image, got {pod:?}"
            );
        }

        // Removing the add-on removes the workload it owns.
        let deleted = call(
            &router,
            crate::certificate_authority::REGION,
            "DELETE",
            &format!(
                "/clusters/{}/addons/{}",
                encode_label(cluster),
                encode_label(addon)
            ),
            "",
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {addon:?}: expected 200 from DeleteAddon, got {deleted:?}"
        );
        let deadline = Instant::now() + DEADLINE;
        loop {
            let read: ApiAnswer = dial(&config, addon, "GET", &path, None).await;
            if read.status == 404 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {addon:?}: the removed add-on must take its workload away, got {read:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
