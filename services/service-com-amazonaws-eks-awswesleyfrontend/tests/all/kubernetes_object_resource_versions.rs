// SPDX-License-Identifier: CC0-1.0

//! The REVISION every object of a cluster's served Kubernetes API carries, and the
//! versioned write a client performs with it.
//!
//! The RULE under test: an object this API server stores carries a server-assigned
//! `metadata.resourceVersion` — "populated by the system, read-only" — which is the
//! revision of the write that last stored it, so two objects never carry one
//! revision and a later write always reports a higher one. A write that carries a
//! `metadata.resourceVersion` is made only while the object still stands at that
//! revision: one carrying the current revision is served, and one carrying a
//! revision the object has moved on from is refused `409` with reason `Conflict`
//! and the sentence apimachinery words a failed optimistic-concurrency check in,
//! leaving the stored object exactly as it was. A write carrying no revision is
//! unconditional.
//!
//! The suite drives that over TABLES whose rows differ along what the rule
//! generalises over: the KIND of object (a Namespace, a ServiceAccount, a Pod, a
//! ConfigMap carrying either of its two value maps, a Deployment, a namespaced Role
//! and a cluster-scoped ClusterRoleBinding), the API GROUP it lives in (the core
//! one, `apps/v1` and `rbac.authorization.k8s.io/v1`), whether it is namespaced, and
//! the FORM of the revision a write carries (the current one, the one the object has
//! moved on from, a revision this store never issued, and none at all).
//!
//! The client is the raw one the Kubernetes API suite uses
//! ([`crate::kubernetes_api::dial`]): a TCP connection to the published port, TLS
//! whose only trust anchor is the published certificate authority, and an HTTP/1.1
//! request written by hand.

use serde_json::json;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, provisioned_cluster, ApiAnswer, Kubeconfig};
use crate::wire::emulator;

/// The namespace the namespaced rows of this suite live in.
const NAMESPACE: &str = "revisions";

/// The revision `document` reports, as the member a client reads it off.
///
/// The member is a STRING on every Kubernetes object, whatever a server counts its
/// revisions in, so a row asserts the form before it compares anything: a present,
/// non-empty string of digits. The number it parses to is what the row orders
/// revisions by.
fn revision_of(case: &str, document: &serde_json::Value) -> u64 {
    let carried = document["metadata"]["resourceVersion"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the object must carry metadata.resourceVersion as a string, got \
                 {:?}",
                document["metadata"]
            )
        });
    assert!(
        !carried.is_empty() && carried.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected a server-assigned revision, got {carried:?}"
    );
    carried.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: the revision {carried:?} must be a number: {error}")
    })
}

/// Create one object through the API and assert it was.
async fn must_create(
    config: &Kubeconfig,
    case: &str,
    path: &str,
    manifest: &serde_json::Value,
) -> ApiAnswer {
    let created = dial(config, case, "POST", path, Some(manifest)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected 201 Created from {path}, got {created:?}"
    );
    created
}

/// The sentence the API server refuses a write of a revision that is no longer the
/// current one with, for the object `resource`/`name` the write addressed.
///
/// The resource is named the way apimachinery names a qualified one: a resource of
/// the core group as it stands, and a resource of another group followed by that
/// group's name.
fn conflict_message(resource: &str, name: &str) -> String {
    format!(
        "Operation cannot be fulfilled on {resource} \"{name}\": the object has been modified; \
         please apply your changes to the latest version and try again"
    )
}

/// A Deployment manifest running `replicas` copies of one container.
fn deployment(name: &str, replicas: u64) -> serde_json::Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name },
        "spec": {
            "replicas": replicas,
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
    })
}

/// A Role manifest naming `verbs` on the ConfigMaps of its own namespace.
fn role(name: &str, verbs: &[&str]) -> serde_json::Value {
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "Role",
        "metadata": { "name": name },
        "rules": [{ "apiGroups": [""], "resources": ["configmaps"], "verbs": verbs }],
    })
}

/// A ClusterRoleBinding manifest binding the published `view` role to `user`.
fn cluster_role_binding(name: &str, user: &str) -> serde_json::Value {
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "ClusterRoleBinding",
        "metadata": { "name": name },
        "roleRef": {
            "apiGroup": "rbac.authorization.k8s.io",
            "kind": "ClusterRole",
            "name": "view",
        },
        "subjects": [{ "kind": "User", "name": user }],
    })
}

/// `manifest` with `revision` carried in its `metadata`, which is how a client asks
/// for the write to be made only while the object still stands there. `None` carries
/// none: the unconditional write.
fn at_revision(manifest: &serde_json::Value, revision: Option<&str>) -> serde_json::Value {
    let mut carried = manifest.clone();
    match revision {
        Some(revision) => carried["metadata"]["resourceVersion"] = json!(revision),
        None => {
            if let Some(metadata) = carried["metadata"].as_object_mut() {
                metadata.remove("resourceVersion");
            }
        }
    }
    carried
}

/// One row of the create table: a kind this API server stores, where its collection
/// lives and whether the object belongs to a namespace.
struct CreateCase {
    label: &'static str,
    collection: String,
    manifest: serde_json::Value,
    name: &'static str,
}

/// One row of the versioned-write table: an object a client may write twice, the
/// resource a refusal names it by, and the two bodies the row writes.
struct WriteCase {
    label: &'static str,
    /// The resource the `409` names, as the refusal words it: the plural resource,
    /// qualified by its API group outside the core one.
    resource: &'static str,
    collection: String,
    name: &'static str,
    /// The manifest the FIRST write stores, and the member that shows it stood.
    first: serde_json::Value,
    /// The manifest the SECOND write stores, which the stale write must not store.
    second: serde_json::Value,
    /// A JSON pointer at the member the two bodies differ in.
    probe: &'static str,
}

#[tokio::test]
async fn every_stored_object_carries_a_revision_and_a_stale_write_is_refused() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "k8s-object-revisions").await;

    must_create(
        &config,
        "the namespace the namespaced rows live in",
        "/api/v1/namespaces",
        &json!({ "metadata": { "name": NAMESPACE } }),
    )
    .await;

    // ------------------------------------------------------------------
    // Every kind this server stores reports the revision it stands at, the
    // revision is STABLE across reads, and no two objects carry one.
    // ------------------------------------------------------------------
    let creates = vec![
        CreateCase {
            label: "a cluster-scoped Namespace",
            collection: "/api/v1/namespaces".to_string(),
            manifest: json!({ "metadata": { "name": "revisions-other" } }),
            name: "revisions-other",
        },
        CreateCase {
            label: "a ServiceAccount",
            collection: format!("/api/v1/namespaces/{NAMESPACE}/serviceaccounts"),
            manifest: json!({ "metadata": { "name": "revision-reader" } }),
            name: "revision-reader",
        },
        CreateCase {
            label: "a Pod",
            collection: format!("/api/v1/namespaces/{NAMESPACE}/pods"),
            manifest: json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "revision-pod" },
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "public.ecr.aws/docker/library/nginx:1.27",
                    }],
                },
            }),
            name: "revision-pod",
        },
        CreateCase {
            label: "a ConfigMap carrying data",
            collection: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            manifest: json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": { "name": "revision-settings" },
                "data": { "color": "blue" },
            }),
            name: "revision-settings",
        },
        CreateCase {
            label: "a ConfigMap carrying binaryData",
            collection: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            manifest: json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": { "name": "revision-binary" },
                "binaryData": { "seed": "AAEC" },
            }),
            name: "revision-binary",
        },
        CreateCase {
            label: "a Deployment of apps/v1",
            collection: format!("/apis/apps/v1/namespaces/{NAMESPACE}/deployments"),
            manifest: deployment("revision-deployment", 1),
            name: "revision-deployment",
        },
        CreateCase {
            label: "a namespaced Role of the RBAC group",
            collection: format!(
                "/apis/rbac.authorization.k8s.io/v1/namespaces/{NAMESPACE}/roles"
            ),
            manifest: role("revision-role", &["get"]),
            name: "revision-role",
        },
        CreateCase {
            label: "a cluster-scoped ClusterRoleBinding of the RBAC group",
            collection: "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings".to_string(),
            manifest: cluster_role_binding("revision-binding", "revision-reader"),
            name: "revision-binding",
        },
    ];

    let mut seen: Vec<(&str, u64)> = Vec::new();
    for case in &creates {
        let label = case.label;
        let created = must_create(&config, label, &case.collection, &case.manifest).await;
        let minted = revision_of(label, &created.body);

        // The revision a read reports is the one the create reported: a read is
        // not a write, so nothing about the object has moved.
        let object = format!("{}/{}", case.collection, case.name);
        for attempt in ["the first read", "the second read"] {
            let read = dial(&config, label, "GET", &object, None).await;
            assert_eq!(
                read.status, 200,
                "case {label:?} ({attempt}): the object must read back, got {read:?}"
            );
            assert_eq!(
                revision_of(label, &read.body),
                minted,
                "case {label:?} ({attempt}): the object stands at the revision its create \
                 reported, got {:?}",
                read.body["metadata"]
            );
        }

        // No two objects of a cluster carry one revision, and a later write
        // carries a higher one than an earlier write.
        for (earlier, revision) in &seen {
            assert!(
                *revision < minted,
                "case {label:?}: expected a revision above {earlier:?}'s {revision}, got {minted}"
            );
        }
        seen.push((label, minted));
    }

    // ------------------------------------------------------------------
    // The versioned write: served at the current revision, refused at any other,
    // unconditional when the manifest carries none.
    // ------------------------------------------------------------------
    let writes = vec![
        WriteCase {
            label: "a ConfigMap of the core group",
            resource: "configmaps",
            collection: format!("/api/v1/namespaces/{NAMESPACE}/configmaps"),
            name: "written-settings",
            first: json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": { "name": "written-settings" },
                "data": { "color": "green" },
            }),
            second: json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": { "name": "written-settings" },
                "data": { "color": "red" },
            }),
            probe: "/data/color",
        },
        WriteCase {
            label: "a Deployment of apps/v1",
            resource: "deployments.apps",
            collection: format!("/apis/apps/v1/namespaces/{NAMESPACE}/deployments"),
            name: "written-deployment",
            first: deployment("written-deployment", 2),
            second: deployment("written-deployment", 3),
            probe: "/spec/replicas",
        },
        WriteCase {
            label: "a namespaced Role of the RBAC group",
            resource: "roles.rbac.authorization.k8s.io",
            collection: format!(
                "/apis/rbac.authorization.k8s.io/v1/namespaces/{NAMESPACE}/roles"
            ),
            name: "written-role",
            first: role("written-role", &["get", "list"]),
            second: role("written-role", &["get", "list", "watch"]),
            probe: "/rules/0/verbs",
        },
        WriteCase {
            label: "a cluster-scoped ClusterRoleBinding of the RBAC group",
            resource: "clusterrolebindings.rbac.authorization.k8s.io",
            collection: "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings".to_string(),
            name: "written-binding",
            first: cluster_role_binding("written-binding", "first-reader"),
            second: cluster_role_binding("written-binding", "second-reader"),
            probe: "/subjects/0/name",
        },
    ];

    for case in &writes {
        let label = case.label;
        let object = format!("{}/{}", case.collection, case.name);
        let created = must_create(
            &config,
            label,
            &case.collection,
            &at_revision(&case.first, None),
        )
        .await;
        let stale = revision_of(label, &created.body);

        // A write carrying the revision the object stands at is SERVED, and the
        // object it answers stands at a new revision.
        let at_current = dial(
            &config,
            label,
            "PUT",
            &object,
            Some(&at_revision(&case.second, Some(&stale.to_string()))),
        )
        .await;
        assert_eq!(
            at_current.status, 200,
            "case {label:?}: a write at the current revision must be served, got {at_current:?}"
        );
        let current = revision_of(label, &at_current.body);
        assert!(
            current > stale,
            "case {label:?}: expected a revision above {stale} after the write, got {current}"
        );
        let expected = case.second.pointer(case.probe).cloned().unwrap_or_default();
        assert_eq!(
            at_current.body.pointer(case.probe).cloned().unwrap_or_default(),
            expected,
            "case {label:?}: the write at the current revision must store {expected:?}, got {:?}",
            at_current.body
        );

        // Any OTHER revision is refused: the one the object has moved on from, and
        // one this store never issued.
        for (form, carried) in [
            ("the revision the object has moved on from", stale.to_string()),
            ("a revision this store never issued", (current + 1_000).to_string()),
        ] {
            let refused = dial(
                &config,
                label,
                "PUT",
                &object,
                Some(&at_revision(&case.first, Some(&carried))),
            )
            .await;
            assert_eq!(
                refused.status, 409,
                "case {label:?} ({form}): the write must be refused 409, got {refused:?}"
            );
            assert_eq!(
                refused.body["kind"],
                json!("Status"),
                "case {label:?} ({form}): the refusal must be a Status, got {:?}",
                refused.body
            );
            assert_eq!(
                refused.body["reason"],
                json!("Conflict"),
                "case {label:?} ({form}): expected reason Conflict, got {:?}",
                refused.body
            );
            assert_eq!(
                refused.body["code"],
                json!(409),
                "case {label:?} ({form}): the Status carries the status code, got {:?}",
                refused.body
            );
            assert_eq!(
                refused.body["message"],
                json!(conflict_message(case.resource, case.name)),
                "case {label:?} ({form}): expected the API server's conflict sentence, got {:?}",
                refused.body["message"]
            );

            // The refused write stored NOTHING: the object still carries what the
            // served write stored, at the revision that write reported.
            let read = dial(&config, label, "GET", &object, None).await;
            assert_eq!(
                read.status, 200,
                "case {label:?} ({form}): the object must still read back, got {read:?}"
            );
            assert_eq!(
                read.body.pointer(case.probe).cloned().unwrap_or_default(),
                expected,
                "case {label:?} ({form}): the refused write must leave {expected:?} stored, got \
                 {:?}",
                read.body
            );
            assert_eq!(
                revision_of(label, &read.body),
                current,
                "case {label:?} ({form}): the refused write must leave the revision at {current}, \
                 got {:?}",
                read.body["metadata"]
            );
        }

        // A write carrying NO revision is unconditional: it is served whatever the
        // object stands at, and moves it on again.
        let unconditional = dial(
            &config,
            label,
            "PUT",
            &object,
            Some(&at_revision(&case.first, None)),
        )
        .await;
        assert_eq!(
            unconditional.status, 200,
            "case {label:?}: a write carrying no revision must be served, got {unconditional:?}"
        );
        let after = revision_of(label, &unconditional.body);
        assert!(
            after > current,
            "case {label:?}: expected a revision above {current} after the unconditional write, \
             got {after}"
        );
        let restored = case.first.pointer(case.probe).cloned().unwrap_or_default();
        assert_eq!(
            unconditional.body.pointer(case.probe).cloned().unwrap_or_default(),
            restored,
            "case {label:?}: the unconditional write must store {restored:?}, got {:?}",
            unconditional.body
        );
    }
}
