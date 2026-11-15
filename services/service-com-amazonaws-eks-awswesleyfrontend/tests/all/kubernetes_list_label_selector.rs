// SPDX-License-Identifier: CC0-1.0

//! WHICH objects a collection of a cluster's Kubernetes API answers with: the
//! `labelSelector` a client restricts a list by.
//!
//! The RULE under test: `labelSelector` is "a selector to restrict the list of
//! returned objects by their labels; defaults to everything", so a collection
//! answers exactly the objects whose `metadata.labels` satisfy EVERY requirement
//! the selector states — and a selector this server cannot read is REFUSED the
//! way the API server refuses an unparsable one (`400`, reason `BadRequest`,
//! naming the requirement it could not read) rather than answered with an empty
//! list, which would tell a client the collection holds nothing matching when in
//! fact its selector was never applied.
//!
//! The reader is the ONE the collections of this API server share
//! (`state::kubernetes_lists`), so the rows below drive it through two different
//! resources — pods and ConfigMaps — and differ along what the rule generalises
//! over: the FORM of each requirement (`key=value`, `key==value`, `key!=value`, a
//! bare key, `!key`, several requirements at once, a key carrying a `/` prefix),
//! and the ways a selector is outside what is read (the set-based half of the
//! grammar, a key outside the label syntax, a value outside it).
//!
//! The client is the kubeconfig-built one the `kubernetes_api` suite dials with,
//! so every request is a real one to the address the cluster published.

use serde_json::json;
use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_lists::MAX_LABEL_REQUIREMENTS;

use crate::certificate_authority::{create_cluster, REGION};
use crate::kubernetes_api::{dial, kubeconfig_of, poll_until_active, ApiAnswer, Kubeconfig};
use crate::wire::emulator;

/// The namespace the objects below live in.
const NAMESPACE: &str = "default";

/// A cluster that has finished provisioning, with the kubeconfig members a client
/// builds its connection from.
async fn published_cluster(name: &str) -> Kubeconfig {
    let router = emulator();
    let created = create_cluster(&router, REGION, name).await;
    assert_eq!(
        created.status, 200,
        "setup: the cluster {name:?} must be created, got {created:?}"
    );
    let described = poll_until_active(&router, REGION, name).await;
    kubeconfig_of(name, &described)
}

/// Create the pod `name` carrying `labels`.
async fn create_pod(config: &Kubeconfig, name: &str, labels: serde_json::Value) {
    let created = dial(
        config,
        name,
        "POST",
        &format!("/api/v1/namespaces/{NAMESPACE}/pods"),
        Some(&json!({
            "metadata": { "name": name, "labels": labels },
            "spec": { "containers": [{ "name": "app", "image": "public.ecr.aws/app:1" }] },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the pod {name:?} must be created, got {created:?}"
    );
}

/// Create the ConfigMap `name` carrying `labels`.
async fn create_config_map(config: &Kubeconfig, name: &str, labels: serde_json::Value) {
    let created = dial(
        config,
        name,
        "POST",
        &format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
        Some(&json!({
            "metadata": { "name": name, "labels": labels },
            "data": { "key": "value" },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the ConfigMap {name:?} must be created, got {created:?}"
    );
}

/// List `resource` under the selector `selector`, as a client asks for it: the
/// selector travels in the query string, so it is sent percent-encoded.
async fn list_under(
    config: &Kubeconfig,
    case: &str,
    resource: &str,
    selector: &str,
) -> ApiAnswer {
    let encoded: String = selector
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    dial(
        config,
        case,
        "GET",
        &format!("/api/v1/namespaces/{NAMESPACE}/{resource}?labelSelector={encoded}"),
        None,
    )
    .await
}

/// The names the answer's `items` carry, in the order they came.
fn listed_names(case: &str, answered: &ApiAnswer) -> Vec<String> {
    answered.body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected items, got {answered:?}"))
        .iter()
        .filter_map(|item| item["metadata"]["name"].as_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// RULE: a collection answers the objects whose labels satisfy every requirement
// the selector states, whatever resource it is and whichever equality-based form
// each requirement takes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_collection_answers_the_objects_the_selector_asks_for() {
    let config = published_cluster("label-selector-cluster").await;

    // Three objects of each resource, carrying the same labels: the selector is
    // the collection reader's, so the rows must hold for both.
    let labelled = [
        ("alpha", json!({ "tier": "web", "eks.amazonaws.com/nodegroup": "workers" })),
        ("beta", json!({ "tier": "batch", "eks.amazonaws.com/nodegroup": "workers" })),
        ("gamma", json!({})),
    ];
    for (name, labels) in &labelled {
        create_pod(&config, name, labels.clone()).await;
        create_config_map(&config, name, labels.clone()).await;
    }

    // (case, the selector, the names it asks for) — one row per form of
    // requirement the grammar's equality half defines, plus two at once.
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        ("one equality requirement", "tier=web", vec!["alpha"]),
        ("the double-equals spelling", "tier==batch", vec!["beta"]),
        (
            "an inequality requirement, which an object carrying no such label satisfies",
            "tier!=web",
            vec!["beta", "gamma"],
        ),
        (
            "a bare key, which requires the label to be set",
            "tier",
            vec!["alpha", "beta"],
        ),
        (
            "a negated key, which requires it not to be",
            "!tier",
            vec!["gamma"],
        ),
        (
            "a key carrying a prefix",
            "eks.amazonaws.com/nodegroup=workers",
            vec!["alpha", "beta"],
        ),
        (
            "two requirements, which an object satisfies both of or neither",
            "eks.amazonaws.com/nodegroup=workers,tier=batch",
            vec!["beta"],
        ),
        (
            "an equality requirement no object satisfies",
            "tier=database",
            vec![],
        ),
    ];

    for resource in ["pods", "configmaps"] {
        for (case, selector, expected) in &rows {
            let answered = list_under(&config, case, resource, selector).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?} on {resource}: a selector this server reads must be served, \
                 got {answered:?}"
            );
            let mut names = listed_names(case, &answered);
            names.sort();
            let mut wanted: Vec<String> =
                expected.iter().map(|name| (*name).to_string()).collect();
            wanted.sort();
            assert_eq!(
                names, wanted,
                "case {case:?} on {resource}: expected {wanted:?}, got {names:?}"
            );
        }
    }

    // And a list naming NO selector asks for everything, which is the parameter's
    // own default.
    let all = dial(
        &config,
        "no selector",
        "GET",
        &format!("/api/v1/namespaces/{NAMESPACE}/pods"),
        None,
    )
    .await;
    let mut names = listed_names("no selector", &all);
    names.sort();
    assert_eq!(
        names,
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "case \"no selector\": a list naming none asks for everything, got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: a selector this server cannot read is refused, not answered with an
// empty list.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_selector_this_server_cannot_read_is_refused() {
    let config = published_cluster("label-selector-refusals").await;
    create_pod(&config, "alpha", json!({ "tier": "web" })).await;
    create_config_map(&config, "alpha", json!({ "tier": "web" })).await;

    // (case, the selector, the part of the refusal detail it must name) — the rows
    // differ along WHY the selector is outside what this server reads: the
    // set-based half of the grammar, a key outside the label syntax, and a value
    // outside it.
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a set-based requirement",
            "tier in (web,batch)",
            "key: Invalid value:",
        ),
        (
            "a negated set-based requirement",
            "tier notin (web)",
            "key: Invalid value:",
        ),
        (
            "a key outside the label syntax",
            "tier/=web",
            "key: Invalid value:",
        ),
        (
            "a key whose prefix is empty",
            "/tier=web",
            "key: Invalid value:",
        ),
        (
            "a value outside the label syntax",
            "tier=-web-",
            "a valid label must be an empty string",
        ),
        (
            "a negated key outside the label syntax",
            "!tier!",
            "key: Invalid value:",
        ),
        (
            "a third equals sign, which no requirement of the grammar carries",
            "tier===web",
            "a valid label must be an empty string",
        ),
    ];

    for resource in ["pods", "configmaps"] {
        for (case, selector, detail) in &rows {
            let answered = list_under(&config, case, resource, selector).await;
            assert_eq!(
                answered.status, 400,
                "case {case:?} on {resource}: a selector this server cannot read must be \
                 refused 400, got {answered:?}"
            );
            assert_eq!(
                answered.body["reason"],
                json!("BadRequest"),
                "case {case:?} on {resource}: expected the BadRequest Status, got {answered:?}"
            );
            let message = answered.body["message"].as_str().unwrap_or_default();
            assert!(
                message.starts_with("unable to parse requirement: "),
                "case {case:?} on {resource}: expected the API server's wording, got \
                 {message:?}"
            );
            assert!(
                message.contains(detail),
                "case {case:?} on {resource}: expected the detail {detail:?}, got {message:?}"
            );
        }
    }

    // And a selector carrying more requirements than are read is refused too,
    // rather than having one of them dropped — dropping a requirement would admit
    // objects the client asked to leave out. The bound is the crate's own, read
    // from it rather than written here.
    let within: Vec<String> = (0..MAX_LABEL_REQUIREMENTS)
        .map(|index| format!("tier{index}!=web"))
        .collect();
    let mut past = within.clone();
    past.push("tier-past!=web".to_string());
    for (case, selector, expected) in [
        ("as many requirements as are read", within.join(","), 200),
        ("one requirement more than are read", past.join(","), 400),
    ] {
        let answered = list_under(&config, case, "pods", &selector).await;
        assert_eq!(
            answered.status, expected,
            "case {case:?}: expected HTTP {expected}, got {answered:?}"
        );
    }
}
