// SPDX-License-Identifier: CC0-1.0

//! The pods a cluster's AWS FARGATE profiles run, as the Kubernetes side of that
//! cluster reports them.
//!
//! The RULE under test: **a Fargate profile says which pods of a cluster run on AWS
//! Fargate — "the selectors to match for a `Pod` to use this Fargate profile. Each
//! selector must have an associated Kubernetes `namespace`. Optionally, you can also
//! specify `labels` for a `namespace`" (`CreateFargateProfileRequest$selectors`) —
//! so a pod an ACTIVE profile selects is placed on the Fargate capacity that profile
//! provides: the cluster provisions a node for that pod alone, registers it ("the Pod
//! execution role allows Fargate infrastructure to register with your cluster as a
//! node", `CreateFargateProfileRequest$podExecutionRoleArn`), binds the pod to it and
//! the pod leaves the phase a pod nothing has taken is in — while a pod NO selector
//! matches is admitted and stays unscheduled on a cluster with no other capacity.**
//!
//! The rows differ along what the rule generalises over: whether a selector names the
//! pod's namespace at all, whether it names labels and whether the pod carries them
//! (exactly, as a superset, at another value, not at all), and which of a cluster's
//! profiles selects the pod. The capacity is also asked the other way round: it is
//! released again once the profile that provided it is gone.
//!
//! Every call under test is a real request: the control plane for the profiles, and
//! the address the cluster published as its `endpoint` for the namespaces, the pods
//! and the nodes.

use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_fargate::{
    fargate_node_name, FARGATE_COMPUTE_TYPE, FARGATE_COMPUTE_TYPE_LABEL, FARGATE_PROFILE_LABEL,
    FARGATE_SCHEDULER_NAME,
};
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_objects::POD_PHASE_PENDING;
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, ApiAnswer, Kubeconfig};
use crate::kubernetes_cluster_resources::published_cluster;
use crate::wire::{call, encode_label, read_until, Answer, POLL_DEADLINE, POLL_INTERVAL};

/// The Pod execution role the profiles of this suite are created with. No IAM
/// service is mounted in this crate's harness, so nothing answers about the role's
/// trust policy and the create is refused on nothing — the cross-service suite
/// `core/router/tests/all/eks_fargate_pod_execution_role_pulls.rs` is where the role
/// really decides a pull.
///
/// Crate-visible, so every suite that gives a cluster Fargate capacity creates its
/// profiles under the same role.
pub(crate) fn pod_execution_role() -> String {
    format!(
        "arn:aws:iam::{}:role/AmazonEKSFargatePodExecutionRole",
        engine::emulator_account_id()
    )
}

/// `CreateFargateProfile` for the profile `name` of `cluster`, carrying `selectors`,
/// waited until the control plane reports it `ACTIVE` — which is when its capacity
/// can take a pod.
///
/// The ONE driver for giving a cluster Fargate capacity, so a suite that needs a
/// cluster whose only capacity is a profile creates it exactly as this one does.
pub(crate) async fn active_profile(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    selectors: &serde_json::Value,
) {
    let created = call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/fargate-profiles", encode_label(cluster)),
        &serde_json::json!({
            "fargateProfileName": name,
            "podExecutionRoleArn": pod_execution_role(),
            "selectors": selectors,
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the profile {name:?} must be created, got {created:?} ({})",
        created.message()
    );
    read_until(
        name,
        "the Fargate profile reporting ACTIVE",
        || describe_profile(router, cluster, name),
        |answer| answer.body["fargateProfile"]["status"] == serde_json::json!("ACTIVE"),
    )
    .await;
}

/// `DescribeFargateProfile` for one profile.
async fn describe_profile(router: &MultiServiceRouter, cluster: &str, name: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/fargate-profiles/{}",
            encode_label(cluster),
            encode_label(name)
        ),
        "",
    )
    .await
}

/// Create the namespace `name` through the published address.
async fn create_namespace(config: &Kubeconfig, name: &str) {
    let created = dial(
        config,
        name,
        "POST",
        "/api/v1/namespaces",
        Some(&serde_json::json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": { "name": name },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the namespace {name:?} must be created, got {created:?}"
    );
}

/// Create the pod `name` in `namespace` carrying `labels`.
async fn create_pod(
    config: &Kubeconfig,
    case: &str,
    namespace: &str,
    name: &str,
    labels: &serde_json::Value,
) -> ApiAnswer {
    let mut metadata = serde_json::json!({ "name": name });
    if !labels.is_null() {
        metadata["labels"] = labels.clone();
    }
    let created = dial(
        config,
        case,
        "POST",
        &format!("/api/v1/namespaces/{namespace}/pods"),
        Some(&serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": metadata,
            "spec": { "containers": [{ "name": "app", "image": "public.ecr.aws/nginx/nginx:1.27" }] },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from the pod create, got {created:?}"
    );
    created
}

/// The `PodScheduled` condition of an answered pod.
fn scheduled_condition(answer: &ApiAnswer) -> serde_json::Value {
    answer.body["status"]["conditions"]
        .as_array()
        .and_then(|conditions| {
            conditions
                .iter()
                .find(|condition| condition["type"] == serde_json::json!("PodScheduled"))
                .cloned()
        })
        .unwrap_or(serde_json::Value::Null)
}

/// One row: the pod the case creates, and the profile whose capacity has to take it
/// — `None` for a pod no selector of the cluster matches.
struct Row {
    case: &'static str,
    namespace: &'static str,
    pod: &'static str,
    labels: serde_json::Value,
    profile: Option<&'static str>,
}

/// The profiles this cluster carries: one selecting a namespace AND labels, one
/// selecting a namespace only.
const LABELLED_PROFILE: &str = "web-profile";
const NAMESPACE_PROFILE: &str = "batch-profile";

#[tokio::test]
async fn a_pod_a_fargate_profile_selects_runs_on_the_capacity_that_profile_provides() {
    let cluster = "fargate-selected-pods";
    let (router, _engine, config) = published_cluster(cluster).await;
    create_namespace(&config, "batch").await;
    active_profile(
        &router,
        cluster,
        LABELLED_PROFILE,
        &serde_json::json!([{ "namespace": "default", "labels": { "tier": "web" } }]),
    )
    .await;
    active_profile(
        &router,
        cluster,
        NAMESPACE_PROFILE,
        &serde_json::json!([{ "namespace": "batch" }]),
    )
    .await;

    let rows = [
        Row {
            case: "the namespace and the label a selector names",
            namespace: "default",
            pod: "selected-exactly",
            labels: serde_json::json!({ "tier": "web" }),
            profile: Some(LABELLED_PROFILE),
        },
        Row {
            case: "more labels than the selector names",
            namespace: "default",
            pod: "selected-superset",
            labels: serde_json::json!({ "tier": "web", "env": "prod", "team": "payments" }),
            profile: Some(LABELLED_PROFILE),
        },
        Row {
            case: "a selector that names the namespace only",
            namespace: "batch",
            pod: "selected-by-namespace",
            labels: serde_json::json!({ "whatever": "goes" }),
            profile: Some(NAMESPACE_PROFILE),
        },
        Row {
            case: "a selector that names the namespace only, and a pod with no label",
            namespace: "batch",
            pod: "selected-unlabelled",
            labels: serde_json::Value::Null,
            profile: Some(NAMESPACE_PROFILE),
        },
        Row {
            case: "the label at a value no selector names",
            namespace: "default",
            pod: "unselected-value",
            labels: serde_json::json!({ "tier": "api" }),
            profile: None,
        },
        Row {
            case: "no label where the selector names one",
            namespace: "default",
            pod: "unselected-unlabelled",
            labels: serde_json::Value::Null,
            profile: None,
        },
        Row {
            case: "a namespace no selector names",
            namespace: "kube-system",
            pod: "unselected-namespace",
            labels: serde_json::json!({ "tier": "web" }),
            profile: None,
        },
    ];

    let mut placed: Vec<String> = Vec::new();
    for row in &rows {
        let Row {
            case,
            namespace,
            pod,
            labels,
            profile,
        } = row;
        let created = create_pod(&config, case, namespace, pod, labels).await;
        let Some(profile) = profile else {
            // A pod no profile selects is admitted, and this cluster has no other
            // capacity to put it on: it stays unscheduled, with the reason
            // Kubernetes gives.
            assert_eq!(
                created.body["spec"]["nodeName"],
                serde_json::Value::Null,
                "case {case:?}: a pod no selector matches is bound to nothing, got {created:?}"
            );
            let condition = scheduled_condition(&created);
            assert_eq!(
                (condition["status"].clone(), condition["reason"].clone()),
                (
                    serde_json::json!("False"),
                    serde_json::json!("Unschedulable")
                ),
                "case {case:?}: the pod reports why it is unscheduled, got {condition:?}"
            );
            assert_eq!(
                created.body["status"]["phase"],
                serde_json::json!(POD_PHASE_PENDING),
                "case {case:?}: a pod nothing has taken has not started, got {created:?}"
            );
            continue;
        };
        // The capacity the profile provides: one node for THIS pod, named after it.
        let node = fargate_node_name(namespace, pod);
        assert_eq!(
            created.body["spec"]["nodeName"],
            serde_json::json!(node),
            "case {case:?}: the pod must be placed on the Fargate capacity of its own, got \
             {created:?}"
        );
        assert_eq!(
            created.body["spec"]["schedulerName"],
            serde_json::json!(FARGATE_SCHEDULER_NAME),
            "case {case:?}: the Fargate scheduler placed it, got {created:?}"
        );
        assert_eq!(
            scheduled_condition(&created)["status"],
            serde_json::json!("True"),
            "case {case:?}: the pod is scheduled, got {created:?}"
        );
        placed.push(node.clone());

        // THE CAPACITY IS REAL: the node the pod names is a node of the cluster, and
        // it reports the profile that provisioned it and the taint that keeps every
        // other pod off it.
        let read = dial(&config, case, "GET", &format!("/api/v1/nodes/{node}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the node the pod was placed on must be readable, got {read:?}"
        );
        assert_eq!(
            (
                read.body["metadata"]["labels"][FARGATE_COMPUTE_TYPE_LABEL].clone(),
                read.body["metadata"]["labels"][FARGATE_PROFILE_LABEL].clone(),
            ),
            (
                serde_json::json!(FARGATE_COMPUTE_TYPE),
                serde_json::json!(*profile),
            ),
            "case {case:?}: the node names the compute type and the profile behind it, got \
             {read:?}"
        );
        assert_eq!(
            read.body["spec"]["taints"],
            serde_json::json!([{
                "key": FARGATE_COMPUTE_TYPE_LABEL,
                "value": FARGATE_COMPUTE_TYPE,
                "effect": "NoSchedule",
            }]),
            "case {case:?}: the node carries the taint a Fargate node carries, got {read:?}"
        );
        assert_eq!(
            read.body["spec"]["providerID"],
            serde_json::Value::Null,
            "case {case:?}: no Amazon EC2 instance is behind Fargate capacity, got {read:?}"
        );

        // AND THE POD LEAVES PENDING: the capacity took it, so it is no longer a pod
        // waiting for somewhere to run. The wait is for that CONDITION, re-read
        // until it holds.
        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        loop {
            let read = dial(
                &config,
                case,
                "GET",
                &format!("/api/v1/namespaces/{namespace}/pods/{pod}"),
                None,
            )
            .await;
            if read.body["status"]["phase"] != serde_json::json!(POD_PHASE_PENDING) {
                assert!(
                    read.body["status"]["startTime"].is_string(),
                    "case {case:?}: the pod reports when the capacity took it, got {read:?}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the pod never left {POD_PHASE_PENDING}; the last answer was \
                 {read:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    // One node per placed pod and no other: Fargate capacity is provisioned for the
    // pods it runs, never for a pod that was not placed on it.
    let nodes = dial(&config, "the cluster's nodes", "GET", "/api/v1/nodes", None).await;
    let mut reported: Vec<String> = nodes.body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|node| node["metadata"]["name"].as_str().map(str::to_string))
        .collect();
    reported.sort();
    let mut expected = placed.clone();
    expected.sort();
    assert_eq!(
        reported, expected,
        "the cluster holds one Fargate node per placed pod, got {nodes:?}"
    );
}

#[tokio::test]
async fn fargate_capacity_is_released_when_its_profile_is_gone() {
    let cluster = "fargate-released-capacity";
    let (router, _engine, config) = published_cluster(cluster).await;
    active_profile(
        &router,
        cluster,
        NAMESPACE_PROFILE,
        &serde_json::json!([{ "namespace": "default" }]),
    )
    .await;

    let case = "the profile that provided the capacity deleted";
    let created = create_pod(&config, case, "default", "released-pod", &serde_json::Value::Null).await;
    let node = fargate_node_name("default", "released-pod");
    assert_eq!(
        created.body["spec"]["nodeName"],
        serde_json::json!(node),
        "case {case:?}: the pod must be placed on the profile's capacity, got {created:?}"
    );

    let deleted = call(
        &router,
        REGION,
        "DELETE",
        &format!(
            "/clusters/{}/fargate-profiles/{}",
            encode_label(cluster),
            encode_label(NAMESPACE_PROFILE)
        ),
        "",
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the profile must be deleted, got {deleted:?} ({})",
        deleted.message()
    );

    // The capacity went with the profile, so the pod is one waiting for capacity
    // again — and the node it ran on is not a node of the cluster any more.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let read = dial(
            &config,
            case,
            "GET",
            "/api/v1/namespaces/default/pods/released-pod",
            None,
        )
        .await;
        if read.body["spec"]["nodeName"].is_null() {
            assert_eq!(
                read.body["status"]["phase"],
                serde_json::json!(POD_PHASE_PENDING),
                "case {case:?}: a pod waiting for capacity reports Pending, got {read:?}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the pod was never released from the gone capacity; the last answer \
             was {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let read = dial(&config, case, "GET", &format!("/api/v1/nodes/{node}"), None).await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: the released capacity is no node of the cluster, got {read:?}"
    );
}
