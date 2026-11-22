// SPDX-License-Identifier: CC0-1.0

//! A managed node group's TAINTS, as the Kubernetes side of the cluster carries
//! them.
//!
//! The RULE under test: a node group's taint reaches the Kubernetes `Node` it is
//! applied to spelled in KUBERNETES's own taint vocabulary — the EKS API takes
//! `NO_SCHEDULE`, `NO_EXECUTE` and `PREFER_NO_SCHEDULE`
//! (`com.amazonaws.eks#TaintEffect`, the enum `Taint$effect` targets) and
//! Kubernetes takes `NoSchedule`, `NoExecute` and `PreferNoSchedule` and nothing
//! else, so the two APIs name the same effect each in its own words — and the
//! taint MEANS to the cluster's scheduler what Kubernetes says it means: a taint
//! whose effect keeps untolerating pods off the node repels a pod that does not
//! tolerate it, and `PreferNoSchedule` does not.
//!
//! The rows differ along what the rule generalises over: the taint effect (all
//! three the EKS API has), and — for the scheduling — the toleration the pod
//! carries (the key and value matched, the key matched with `Exists`, every taint
//! tolerated with a key-less `Exists`, a value that does not match, an effect that
//! does not match, and no toleration at all).
//!
//! The nodes a node group registered are the instances the Amazon EC2 services
//! launched for it, and no Amazon EC2 service is mounted in this crate's harness —
//! so the rows write them into the same `NodegroupCapacity::nodes` the node-group
//! create fills from those services, through the crate's public `state` module.
//! Every call under test is a real request: a control-plane call for the node
//! group, and the published Kubernetes API address for the node and the pods.

use std::time::{Duration, Instant};

use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_nodes::kubernetes_taint_effect;
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_pod_spec::MAX_TOLERATIONS_PER_POD;
use service_com_amazonaws_eks_awswesleyfrontend::state::{
    too_many_taints_message, MAX_TAINTS_PER_NODEGROUP,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, ApiAnswer, Kubeconfig};
use crate::kubernetes_cluster_resources::{instance, published_cluster, register_nodes};
use crate::nodegroup_lifecycle::create_nodegroup_in;

/// How long a polled condition is given before the case fails. Generous: the one
/// span these cases wait out is the service's own node-group provisioning, and
/// nothing here measures elapsed time.
const DEADLINE: Duration = Duration::from_secs(60);

/// The node group every row creates its capacity under.
const NODEGROUP: &str = "tainted-workers";

/// The three `TaintEffect` values the EKS API takes, each with the Kubernetes
/// taint effect it is the EKS spelling of, and whether that effect keeps a pod
/// that does not tolerate the taint off the node.
///
/// The catalog's source is the enum `Taint$effect` targets
/// (`smithy_query(query="shape", shape="com.amazonaws.eks#TaintEffect")` lists
/// `NO_EXECUTE`, `NO_SCHEDULE`, `PREFER_NO_SCHEDULE`) and the three values
/// Kubernetes accepts in `Node.spec.taints[].effect`.
const TAINT_EFFECTS: [(&str, &str, bool); 3] = [
    ("NO_SCHEDULE", "NoSchedule", true),
    ("NO_EXECUTE", "NoExecute", true),
    ("PREFER_NO_SCHEDULE", "PreferNoSchedule", false),
];

/// A cluster whose node group applies the taint `(key, value, effect)` to one
/// registered node, with the kubeconfig a client dials it by and the name that
/// node registered under.
async fn tainted_cluster(
    cluster: &str,
    key: &str,
    value: &str,
    effect: &str,
) -> (MultiServiceRouter, Kubeconfig, String) {
    let (router, engine, config) = published_cluster(cluster).await;
    let created = create_nodegroup_in(
        &router,
        REGION,
        cluster,
        &serde_json::json!({
            "nodegroupName": NODEGROUP,
            "subnets": ["subnet-aaaa1111", "subnet-bbbb2222"],
            "nodeRole": "arn:aws:iam::000000000000:role/eksNodeRole",
            "taints": [{ "key": key, "value": value, "effect": effect }],
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "cluster {cluster:?}: expected 200 from CreateNodegroup, got {created:?}"
    );
    // The node group registers its nodes with the cluster once it is ACTIVE, which
    // the clock owes it; the wait is for that CONDITION.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = describe_nodegroup(&router, cluster).await;
        if described.body["nodegroup"]["status"] == serde_json::json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cluster {cluster:?}: the node group never reported ACTIVE; the last answer was \
             {described:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let node = instance("i-0d4d4d4d4d4d4d4d4", "us-west-2a", "t3.medium", "10.0.4.44");
    register_nodes(&engine, cluster, NODEGROUP, std::slice::from_ref(&node));
    let name = node.node_name().to_string();
    (router, config, name)
}

/// `DescribeNodegroup` for the node group the rows create.
async fn describe_nodegroup(router: &MultiServiceRouter, cluster: &str) -> crate::wire::Answer {
    crate::wire::call(
        router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/node-groups/{}",
            crate::wire::encode_label(cluster),
            crate::wire::encode_label(NODEGROUP)
        ),
        "",
    )
    .await
}

/// Create the pod `name` in the `default` namespace, carrying `tolerations`.
async fn create_pod(
    config: &Kubeconfig,
    case: &str,
    name: &str,
    tolerations: &serde_json::Value,
) -> ApiAnswer {
    let mut spec = serde_json::json!({
        "containers": [{ "name": "app", "image": "nginx:1.27" }],
    });
    if !tolerations.is_null() {
        spec["tolerations"] = tolerations.clone();
    }
    let created = dial(
        config,
        case,
        "POST",
        "/api/v1/namespaces/default/pods",
        Some(&serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": { "name": name },
            "spec": spec,
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from the pod create, got {created:?}"
    );
    created
}

/// RULE: the effect the EKS API names a node group's taint by reaches the
/// Kubernetes node spelled as Kubernetes spells that same effect, while the EKS
/// API keeps reporting its own spelling.
#[tokio::test]
async fn a_node_group_taint_reaches_the_node_in_the_kubernetes_vocabulary() {
    for (api_effect, kubernetes_effect, _) in TAINT_EFFECTS {
        let cluster = format!("taint-{}", api_effect.to_ascii_lowercase().replace('_', "-"));
        let key = "dedicated";
        let value = "batch";
        let (router, config, node_name) = tainted_cluster(&cluster, key, value, api_effect).await;

        let read = dial(
            &config,
            api_effect,
            "GET",
            &format!("/api/v1/nodes/{node_name}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {api_effect:?}: the node must be readable, got {read:?}"
        );
        assert_eq!(
            read.body["spec"]["taints"],
            serde_json::json!([{
                "key": key,
                "value": value,
                "effect": kubernetes_effect,
            }]),
            "case {api_effect:?}: the node carries the taint in the Kubernetes vocabulary, got \
             {read:?}"
        );

        // The EKS API still reports the taint in ITS vocabulary: one taint, two
        // APIs, each spelling it its own way.
        let described = describe_nodegroup(&router, &cluster).await;
        assert_eq!(
            described.body["nodegroup"]["taints"],
            serde_json::json!([{ "key": key, "value": value, "effect": api_effect }]),
            "case {api_effect:?}: DescribeNodegroup reports the EKS spelling, got {described:?}"
        );

        // And the translation itself is the one that carries each enum value onto
        // its Kubernetes spelling.
        let translated = kubernetes_taint_effect(api_effect);
        assert_eq!(
            translated, kubernetes_effect,
            "case {api_effect:?}: expected the Kubernetes spelling {kubernetes_effect:?}, got \
             {translated:?}"
        );
    }
}

/// RULE: a node whose taint repels untolerating pods takes only the pods that
/// tolerate that taint; `PreferNoSchedule` repels none.
#[tokio::test]
async fn a_tainted_node_takes_only_the_pods_that_tolerate_its_taint() {
    let key = "workload";
    let value = "gpu";
    for (api_effect, kubernetes_effect, repels) in TAINT_EFFECTS {
        let cluster = format!(
            "tolerate-{}",
            api_effect.to_ascii_lowercase().replace('_', "-")
        );
        let (_router, config, node_name) = tainted_cluster(&cluster, key, value, api_effect).await;

        // (the case, the pod's name, the tolerations it carries, whether the node
        // takes it when the taint repels untolerating pods)
        let rows: Vec<(&str, &str, serde_json::Value, bool)> = vec![
            ("no toleration at all", "no-toleration", serde_json::Value::Null, false),
            (
                "the key and the value matched",
                "matched-value",
                serde_json::json!([{
                    "key": key,
                    "operator": "Equal",
                    "value": value,
                    "effect": kubernetes_effect,
                }]),
                true,
            ),
            (
                "the key matched with no operator stated",
                "default-operator",
                serde_json::json!([{ "key": key, "value": value }]),
                true,
            ),
            (
                "the key tolerated whatever its value",
                "exists-key",
                serde_json::json!([{ "key": key, "operator": "Exists" }]),
                true,
            ),
            (
                "every taint tolerated",
                "exists-everything",
                serde_json::json!([{ "operator": "Exists" }]),
                true,
            ),
            (
                "a value that is not the taint's",
                "other-value",
                serde_json::json!([{ "key": key, "operator": "Equal", "value": "cpu" }]),
                false,
            ),
            (
                "an effect that is not the taint's",
                "other-effect",
                serde_json::json!([{
                    "key": key,
                    "operator": "Exists",
                    "effect": other_effect(kubernetes_effect),
                }]),
                false,
            ),
            (
                "another key's taint tolerated",
                "other-key",
                serde_json::json!([{ "key": "elsewhere", "operator": "Exists" }]),
                false,
            ),
        ];
        for (case, pod_name, tolerations, tolerated) in &rows {
            let case = format!("{api_effect}: {case}");
            let created = create_pod(&config, &case, pod_name, tolerations).await;
            // A node whose taint repels untolerating pods takes the pod only when
            // the pod tolerates it; a `PreferNoSchedule` taint takes every pod.
            let bound = *tolerated || !repels;
            let expected = if bound {
                serde_json::json!(node_name)
            } else {
                serde_json::Value::Null
            };
            assert_eq!(
                created.body["spec"]["nodeName"], expected,
                "case {case:?}: expected the pod bound to {expected:?}, got {created:?}"
            );
            let condition = created.body["status"]["conditions"]
                .as_array()
                .and_then(|conditions| {
                    conditions
                        .iter()
                        .find(|condition| condition["type"] == serde_json::json!("PodScheduled"))
                        .cloned()
                })
                .unwrap_or(serde_json::Value::Null);
            assert_eq!(
                condition["status"],
                serde_json::json!(if bound { "True" } else { "False" }),
                "case {case:?}: expected PodScheduled {}, got {condition:?}",
                if bound { "True" } else { "False" }
            );
        }
    }
}

/// One Kubernetes taint effect that is NOT `effect`, for the row whose toleration
/// names an effect the node's taint does not carry.
fn other_effect(effect: &str) -> &'static str {
    TAINT_EFFECTS
        .into_iter()
        .map(|(_, kubernetes_effect, _)| kubernetes_effect)
        .find(|kubernetes_effect| *kubernetes_effect != effect)
        .unwrap_or("NoSchedule")
}

/// RULE: the two lists the scheduler matches — the node group's taints and the
/// pod's tolerations — are each held to the length their own API holds them to
/// before anything is stored: a list AT the bound is accepted, one past it is
/// refused, and the refusal names the bound.
#[tokio::test]
async fn the_lists_the_scheduler_matches_are_held_to_their_bounds() {
    let (router, _engine, config) = published_cluster("taint-bounds-cluster").await;

    // The taints of one node group, at Amazon EKS's quota for them and one past
    // it. `MAX_TAINTS_PER_NODEGROUP` is the published bound; the row past it is
    // the next value up.
    let taints = |count: usize| -> serde_json::Value {
        (0..count)
            .map(|index| {
                serde_json::json!({
                    "key": format!("key{index}"),
                    "value": "batch",
                    "effect": "NO_SCHEDULE",
                })
            })
            .collect()
    };
    // (the case, the node group's name, how many taints it asks for, whether the
    // create is accepted)
    let rows = [
        ("at the quota", "at-quota", MAX_TAINTS_PER_NODEGROUP, true),
        ("past the quota", "past-quota", MAX_TAINTS_PER_NODEGROUP + 1, false),
    ];
    for (case, name, count, accepted) in rows {
        let answer = create_nodegroup_in(
            &router,
            REGION,
            "taint-bounds-cluster",
            &serde_json::json!({
                "nodegroupName": name,
                "subnets": ["subnet-aaaa1111", "subnet-bbbb2222"],
                "nodeRole": "arn:aws:iam::000000000000:role/eksNodeRole",
                "taints": taints(count),
            }),
        )
        .await;
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200 from CreateNodegroup, got {answer:?}"
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400 from CreateNodegroup, got {answer:?}"
        );
        assert_eq!(
            answer.error_type.as_deref(),
            Some("InvalidParameterException"),
            "case {case:?}: expected InvalidParameterException, got {answer:?}"
        );
        let message = answer.body["message"].as_str().unwrap_or_default().to_string();
        assert!(
            message.contains(&too_many_taints_message(count)),
            "case {case:?}: expected the refusal to name the bound and the count, got \
             {message:?}"
        );
        // A refused create stores nothing.
        let described = describe_nodegroup_named(&router, "taint-bounds-cluster", name).await;
        assert_eq!(
            described.status, 404,
            "case {case:?}: a refused create must store nothing, got {described:?}"
        );
    }

    // The quota holds for the list a CHANGE would leave the node group carrying:
    // the node group accepted above is already at it, so adding one more taint is
    // refused, while a change that removes one and adds another is not.
    //
    // (the case, the taint change, whether the update is accepted)
    let update_rows: Vec<(&str, serde_json::Value, bool)> = vec![
        (
            "a change naming more taints than a node group may carry",
            serde_json::json!({
                "addOrUpdateTaints": (0..=MAX_TAINTS_PER_NODEGROUP)
                    .map(|index| serde_json::json!({
                        "key": format!("named{index}"),
                        "value": "batch",
                        "effect": "NO_SCHEDULE",
                    }))
                    .collect::<Vec<serde_json::Value>>(),
            }),
            false,
        ),
        (
            "a removal naming more taints than a node group may carry",
            serde_json::json!({
                "removeTaints": (0..=MAX_TAINTS_PER_NODEGROUP)
                    .map(|index| serde_json::json!({
                        "key": format!("named{index}"),
                        "value": "batch",
                        "effect": "NO_SCHEDULE",
                    }))
                    .collect::<Vec<serde_json::Value>>(),
            }),
            false,
        ),
        (
            "one more taint past the quota",
            serde_json::json!({
                "addOrUpdateTaints": [
                    { "key": "extra", "value": "batch", "effect": "NO_SCHEDULE" },
                ],
            }),
            false,
        ),
        (
            "a taint replaced at the same key and effect",
            serde_json::json!({
                "addOrUpdateTaints": [
                    { "key": "key0", "value": "interactive", "effect": "NO_SCHEDULE" },
                ],
            }),
            true,
        ),
        (
            "one removed and another added",
            serde_json::json!({
                "addOrUpdateTaints": [
                    { "key": "swapped", "value": "batch", "effect": "NO_SCHEDULE" },
                ],
                "removeTaints": [
                    { "key": "key1", "value": "batch", "effect": "NO_SCHEDULE" },
                ],
            }),
            true,
        ),
    ];
    for (case, taints_change, accepted) in &update_rows {
        // A node group takes a change only once it has SETTLED, and only one at a
        // time: the wait is for that CONDITION, re-read until it holds.
        let deadline = Instant::now() + DEADLINE;
        loop {
            let described =
                describe_nodegroup_named(&router, "taint-bounds-cluster", "at-quota").await;
            if described.body["nodegroup"]["status"] == serde_json::json!("ACTIVE") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the node group never settled; the last answer was {described:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let answer = crate::wire::call(
            &router,
            REGION,
            "POST",
            &format!(
                "/clusters/{}/node-groups/{}/update-config",
                crate::wire::encode_label("taint-bounds-cluster"),
                crate::wire::encode_label("at-quota")
            ),
            &serde_json::json!({ "taints": taints_change }).to_string(),
        )
        .await;
        if *accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200 from UpdateNodegroupConfig, got {answer:?}"
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400 from UpdateNodegroupConfig, got {answer:?}"
        );
        assert_eq!(
            answer.error_type.as_deref(),
            Some("InvalidParameterException"),
            "case {case:?}: expected InvalidParameterException, got {answer:?}"
        );
        let message = answer.body["message"].as_str().unwrap_or_default().to_string();
        assert!(
            message.contains(&too_many_taints_message(MAX_TAINTS_PER_NODEGROUP + 1)),
            "case {case:?}: expected the refusal to name the bound and the count the change \
             would leave, got {message:?}"
        );
    }

    // The pod's own list, at the bound this API server holds a pod to and one past
    // it, refused with the validation `Status` apimachinery refuses an over-long
    // list with.
    let tolerations = |count: usize| -> serde_json::Value {
        (0..count)
            .map(|index| {
                serde_json::json!({ "key": format!("key{index}"), "operator": "Exists" })
            })
            .collect()
    };
    for (case, pod_name, count, accepted) in [
        ("at the bound", "at-bound", MAX_TOLERATIONS_PER_POD, true),
        ("past the bound", "past-bound", MAX_TOLERATIONS_PER_POD + 1, false),
    ] {
        let answered = dial(
            &config,
            case,
            "POST",
            "/api/v1/namespaces/default/pods",
            Some(&serde_json::json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": pod_name },
                "spec": {
                    "containers": [{ "name": "app", "image": "nginx:1.27" }],
                    "tolerations": tolerations(count),
                },
            })),
        )
        .await;
        if accepted {
            assert_eq!(
                answered.status, 201,
                "case {case:?}: expected HTTP 201 from the pod create, got {answered:?}"
            );
            continue;
        }
        assert_eq!(
            answered.status, 422,
            "case {case:?}: expected HTTP 422 from the pod create, got {answered:?}"
        );
        let message = answered.body["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(&format!(
                "spec.tolerations: Too many: {count}: must have at most \
                 {MAX_TOLERATIONS_PER_POD} items"
            )),
            "case {case:?}: expected the refusal to name the bound and the count, got \
             {message:?}"
        );
        // And nothing is stored for the refused pod.
        let read = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/default/pods/{pod_name}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused pod create must store nothing, got {read:?}"
        );
    }
}

/// `DescribeNodegroup` for a node group named by the row.
async fn describe_nodegroup_named(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
) -> crate::wire::Answer {
    crate::wire::call(
        router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/node-groups/{}",
            crate::wire::encode_label(cluster),
            crate::wire::encode_label(name)
        ),
        "",
    )
    .await
}
