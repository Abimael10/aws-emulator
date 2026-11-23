// SPDX-License-Identifier: CC0-1.0

//! The VERBS the Kubernetes API server a cluster publishes serves on its core
//! group, and the object it answers a create with.
//!
//! The RULE under test: the resource list that API server publishes at `/api/v1`
//! is the contract its routing keeps — a collection serves every verb its own row
//! advertises, and `405 MethodNotAllowed` is the answer only for a verb no row
//! advertises, worded as the API server words one ("the server does not allow
//! <VERB> on <path>"). A Kubernetes client resolves the kind it was asked for
//! against that document before it addresses anything, and a `405` tells it the
//! verb is unsupported by the resource — a permanent, non-retryable answer — so a
//! collection registered for fewer verbs than it advertises makes the client give
//! up on a verb the server itself publishes. `Cluster$endpoint` is "the endpoint
//! for your Kubernetes API server", so the documented thing to do with it is to
//! talk Kubernetes there and get the Kubernetes answer.
//!
//! The second suite pins what a create then answers with: the object that was
//! STORED, carrying the members its kind requires and readable again at its own
//! address. `PodSpec.containers` is the one member the Kubernetes API requires of
//! every Pod ("there must be at least one container in a Pod"), so a Pod answered
//! without it is an object a typed client cannot construct at all.
//!
//! Each suite drives the rule over a TABLE whose rows differ along what it
//! generalises over: which resource of the served catalog is addressed, whether it
//! lives in a namespace, whether it is a collection or a subresource of one
//! object, which verb of the five is asked for — and, for the created objects, the
//! KIND created, the namespace it is created in (one Kubernetes creates with the
//! cluster, one the client created) and how much the manifest carries.
//!
//! The client is the raw, kubeconfig-built one [`crate::kubernetes_api::dial`]
//! dials with: a TCP connection to the published address, TLS verified against
//! nothing but the published certificate authority, and one HTTP/1.1 exchange.

use serde_json::json;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, emulator_with_pod_identity, provisioned_cluster, PODS_AUDIENCE};

/// The verbs each row of the catalog is probed on: the four the served resource
/// list advertises across its rows (`create`, `get`, `list`, `update`) and
/// `delete`, which no row of the core group advertises — so every row is probed
/// both on verbs it advertises and on verbs it does not.
const PROBED_VERBS: [&str; 5] = ["create", "get", "list", "update", "delete"];

/// How ONE resource of the served catalog is addressed, verb by verb.
struct ResourceProbe {
    /// The `APIResource.name` of the row this probe is for.
    resource: &'static str,
    /// Where a create is addressed: the resource's collection, or — for a
    /// subresource — the subresource of one object.
    create_path: &'static str,
    /// The body a create and an update carry.
    manifest: serde_json::Value,
    /// Where a list is addressed.
    list_path: &'static str,
    /// Where the verbs that address ONE object are addressed.
    object_path: &'static str,
}

impl ResourceProbe {
    /// The `(method, path)` `verb` is asked for with.
    fn addressed(&self, verb: &str) -> (&'static str, &'static str) {
        match verb {
            "create" => ("POST", self.create_path),
            "list" => ("GET", self.list_path),
            "update" => ("PUT", self.object_path),
            "delete" => ("DELETE", self.object_path),
            _ => ("GET", self.object_path),
        }
    }

    /// The body `verb` carries: the manifest for the two verbs that submit an
    /// object, and nothing for the three that only address one.
    fn body(&self, verb: &str) -> Option<&serde_json::Value> {
        matches!(verb, "create" | "update").then_some(&self.manifest)
    }
}

/// The status an advertised verb answers: a create answers the object it stored,
/// a list answers the collection, and a verb that addresses ONE object answers it
/// when there is one and the API server's not-found `Status` when there is not —
/// never the `405` of a verb the resource does not serve.
fn expected_status(verb: &str, object_exists: bool) -> u16 {
    match verb {
        "create" => 201,
        "list" => 200,
        _ if object_exists => 200,
        _ => 404,
    }
}

/// One probe per resource of the served catalog. The namespace is `default`, which
/// Kubernetes creates with the cluster, and the service account the token
/// subresource is addressed under is that namespace's own `default` account.
fn probes() -> Vec<ResourceProbe> {
    vec![
        ResourceProbe {
            resource: "namespaces",
            create_path: "/api/v1/namespaces",
            manifest: json!({ "metadata": { "name": "verb-probe" } }),
            list_path: "/api/v1/namespaces",
            object_path: "/api/v1/namespaces/verb-probe",
        },
        ResourceProbe {
            resource: "nodes",
            create_path: "/api/v1/nodes",
            manifest: json!({ "metadata": { "name": "verb-probe-node" } }),
            list_path: "/api/v1/nodes",
            // A cluster with no node group registered none, so the node this
            // addresses is one the cluster does not hold.
            object_path: "/api/v1/nodes/verb-probe-node",
        },
        ResourceProbe {
            resource: "pods",
            create_path: "/api/v1/namespaces/default/pods",
            manifest: json!({
                "metadata": { "name": "verb-probe-pod" },
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "public.ecr.aws/docker/library/nginx:1.27",
                    }],
                },
            }),
            list_path: "/api/v1/namespaces/default/pods",
            object_path: "/api/v1/namespaces/default/pods/verb-probe-pod",
        },
        ResourceProbe {
            resource: "configmaps",
            create_path: "/api/v1/namespaces/default/configmaps",
            manifest: json!({
                "metadata": { "name": "verb-probe-settings" },
                "data": { "log-level": "debug" },
            }),
            list_path: "/api/v1/namespaces/default/configmaps",
            object_path: "/api/v1/namespaces/default/configmaps/verb-probe-settings",
        },
        ResourceProbe {
            resource: "events",
            create_path: "/api/v1/namespaces/default/events",
            manifest: json!({ "metadata": { "name": "verb-probe-event" } }),
            list_path: "/api/v1/namespaces/default/events",
            // The cluster's own components record events; nothing recorded one
            // about an object of this name.
            object_path: "/api/v1/namespaces/default/events/verb-probe-event",
        },
        ResourceProbe {
            resource: "serviceaccounts",
            create_path: "/api/v1/namespaces/default/serviceaccounts",
            manifest: json!({ "metadata": { "name": "verb-probe-account" } }),
            list_path: "/api/v1/namespaces/default/serviceaccounts",
            object_path: "/api/v1/namespaces/default/serviceaccounts/verb-probe-account",
        },
        ResourceProbe {
            resource: "serviceaccounts/token",
            create_path: "/api/v1/namespaces/default/serviceaccounts/default/token",
            manifest: json!({ "spec": { "audiences": [PODS_AUDIENCE] } }),
            list_path: "/api/v1/namespaces/default/serviceaccounts/default/token",
            object_path: "/api/v1/namespaces/default/serviceaccounts/default/token",
        },
    ]
}

#[tokio::test]
async fn every_verb_the_core_api_advertises_is_served() {
    let (router, _engine) = emulator_with_pod_identity();
    let config = provisioned_cluster(&router, REGION, "k8s-core-verbs").await;

    let listed = dial(&config, "the core group's resources", "GET", "/api/v1", None).await;
    assert_eq!(
        listed.status, 200,
        "the core group's resource list must be served, got {listed:?}"
    );
    let rows = listed.body["resources"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !rows.is_empty(),
        "the resource list must name the resources the server serves, got {listed:?}"
    );

    let probes = probes();
    // The table spans the WHOLE catalog: a resource the server advertises and
    // this table does not probe is a resource whose verbs nothing checks.
    for row in &rows {
        let name = row["name"].as_str().unwrap_or_default();
        assert!(
            probes.iter().any(|probe| probe.resource == name),
            "case {name:?}: every resource the server advertises must be probed, got {rows:?}"
        );
    }

    for probe in &probes {
        let case = probe.resource;
        let row = rows
            .iter()
            .find(|row| row["name"] == json!(case))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the resource list must advertise it, got {rows:?}")
            });
        let verbs: Vec<String> = row["verbs"]
            .as_array()
            .map(|verbs| {
                verbs
                    .iter()
                    .filter_map(|verb| verb.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !verbs.is_empty(),
            "case {case:?}: a resource is advertised with the verbs it serves, got {row:?}"
        );
        let advertises = |verb: &str| verbs.iter().any(|served| served == verb);

        // The object the rest of the probes address, where the resource is one a
        // client creates at all: the create is the verb that makes it, so it runs
        // first and is held to the status a create answers.
        let creatable = advertises("create");
        if creatable {
            let (method, path) = probe.addressed("create");
            let created = dial(&config, case, method, path, probe.body("create")).await;
            assert_eq!(
                created.status,
                expected_status("create", false),
                "case {case:?}: the create verb the server advertises must be served at \
                 {path:?}, got {created:?}"
            );
        }

        for verb in PROBED_VERBS {
            if verb == "create" && creatable {
                continue;
            }
            let (method, path) = probe.addressed(verb);
            let answered = dial(&config, case, method, path, probe.body(verb)).await;
            if advertises(verb) {
                assert_ne!(
                    answered.status, 405,
                    "case {case:?}: {verb:?} is advertised, so {method} {path} must not be \
                     refused as a verb the resource does not serve, got {answered:?}"
                );
                assert_eq!(
                    answered.status,
                    expected_status(verb, creatable),
                    "case {case:?}: expected the answer {verb:?} serves at {path:?}, got \
                     {answered:?}"
                );
            } else {
                assert_eq!(
                    answered.status, 405,
                    "case {case:?}: {verb:?} is not advertised, so {method} {path} is a verb \
                     the resource does not serve, got {answered:?}"
                );
                assert_eq!(
                    answered.body["kind"],
                    json!("Status"),
                    "case {case:?}: a refusal is a Status object, got {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.body["reason"],
                    json!("MethodNotAllowed"),
                    "case {case:?}: expected the reason of a verb the resource does not \
                     serve, got {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.body["code"],
                    json!(405),
                    "case {case:?}: the Status carries the status it was answered with, got \
                     {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.body["message"],
                    json!(format!("the server does not allow {method} on {path}")),
                    "case {case:?}: expected the sentence the API server words it in, got {:?}",
                    answered.body
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: the object a create is answered with is the object that was STORED —
// carrying the members its kind requires, and reading back unchanged at its own
// address.
// ---------------------------------------------------------------------------

/// One row of the created-object table.
struct CreatedObjectCase {
    label: &'static str,
    /// A namespace the client creates before the object, for a row created in one
    /// Kubernetes did not create with the cluster.
    namespace: Option<&'static str>,
    /// The collection the create is addressed at.
    collection: &'static str,
    /// Where the created object reads back.
    object_path: &'static str,
    manifest: serde_json::Value,
    /// The `kind` the answer carries.
    kind: &'static str,
    /// The members the Kubernetes API requires of this kind, as JSON Pointers into
    /// the object the create answers with.
    required: &'static [&'static str],
}

#[tokio::test]
async fn a_created_object_carries_the_members_its_kind_requires() {
    let (router, _engine) = emulator_with_pod_identity();
    let config = provisioned_cluster(&router, REGION, "k8s-created-objects").await;

    // The rows differ along what the rule generalises over: the KIND created, the
    // namespace it is created in (one Kubernetes creates with the cluster, one the
    // client created), how many containers a pod declares and what else its spec
    // carries.
    let cases = [
        CreatedObjectCase {
            label: "a pod that runs one image",
            namespace: None,
            collection: "/api/v1/namespaces/default/pods",
            object_path: "/api/v1/namespaces/default/pods/one-container",
            manifest: json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "one-container" },
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "000000000000.dkr.ecr.us-west-2.amazonaws.com/app:1.4",
                    }],
                },
            }),
            kind: "Pod",
            required: &[
                "/metadata/name",
                "/metadata/namespace",
                "/metadata/uid",
                "/metadata/creationTimestamp",
                "/spec/containers",
                "/status/phase",
            ],
        },
        CreatedObjectCase {
            label: "a pod whose containers carry pull secrets and an environment",
            namespace: None,
            collection: "/api/v1/namespaces/default/pods",
            object_path: "/api/v1/namespaces/default/pods/two-containers",
            manifest: json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": "two-containers" },
                "spec": {
                    "containers": [
                        {
                            "name": "app",
                            "image": "000000000000.dkr.ecr.us-west-2.amazonaws.com/app",
                            "env": [{ "name": "AWS_REGION", "value": REGION }],
                        },
                        {
                            "name": "sidecar",
                            "image": "public.ecr.aws/aws-cli/aws-cli:latest",
                        },
                    ],
                    "imagePullSecrets": [{ "name": "registry-credentials" }],
                },
            }),
            kind: "Pod",
            required: &[
                "/metadata/name",
                "/metadata/uid",
                "/spec/containers",
                "/spec/imagePullSecrets",
                "/status/phase",
            ],
        },
        CreatedObjectCase {
            label: "the log-routing ConfigMap in a namespace the client created",
            namespace: Some("aws-observability"),
            collection: "/api/v1/namespaces/aws-observability/configmaps",
            object_path: "/api/v1/namespaces/aws-observability/configmaps/aws-logging",
            manifest: json!({
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": { "name": "aws-logging" },
                "data": {
                    "flb_log_cw": "true",
                    "output.conf": "[OUTPUT]\n    Name cloudwatch_logs\n    Match   *\n",
                },
            }),
            kind: "ConfigMap",
            required: &[
                "/metadata/name",
                "/metadata/namespace",
                "/metadata/uid",
                "/data",
            ],
        },
        CreatedObjectCase {
            label: "a service account a workload runs as",
            namespace: None,
            collection: "/api/v1/namespaces/default/serviceaccounts",
            object_path: "/api/v1/namespaces/default/serviceaccounts/workload",
            manifest: json!({
                "apiVersion": "v1",
                "kind": "ServiceAccount",
                "metadata": { "name": "workload" },
            }),
            kind: "ServiceAccount",
            required: &["/metadata/name", "/metadata/namespace", "/metadata/uid"],
        },
        CreatedObjectCase {
            label: "a namespace of its own",
            namespace: None,
            collection: "/api/v1/namespaces",
            object_path: "/api/v1/namespaces/team-apps",
            manifest: json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": "team-apps" },
            }),
            kind: "Namespace",
            required: &["/metadata/name", "/metadata/uid", "/status/phase"],
        },
    ];

    for case in &cases {
        let label = case.label;
        if let Some(namespace) = case.namespace {
            let created = dial(
                &config,
                label,
                "POST",
                "/api/v1/namespaces",
                Some(&json!({ "metadata": { "name": namespace } })),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {label:?}: expected the namespace to be created, got {created:?}"
            );
        }
        let created = dial(&config, label, "POST", case.collection, Some(&case.manifest)).await;
        assert_eq!(
            created.status, 201,
            "case {label:?}: the collection must serve the create, got {created:?}"
        );
        assert_eq!(
            created.body["kind"],
            json!(case.kind),
            "case {label:?}: expected a {} object, got {:?}",
            case.kind,
            created.body
        );
        for path in case.required {
            assert!(
                created
                    .body
                    .pointer(path)
                    .is_some_and(|value| !value.is_null()),
                "case {label:?}: the object must carry {path}, got {:?}",
                created.body
            );
        }

        // The containers are the member the Kubernetes API requires of a Pod, so
        // the list is there, non-empty, and every entry names a container and the
        // image it runs — exactly as the manifest declared them.
        if case.kind == "Pod" {
            let submitted = case.manifest["spec"]["containers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let containers = created.body["spec"]["containers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                containers.len(),
                submitted.len(),
                "case {label:?}: expected {} container(s), got {containers:?}",
                submitted.len()
            );
            assert!(
                !containers.is_empty(),
                "case {label:?}: a Pod carries at least one container, got {containers:?}"
            );
            for (index, entry) in containers.iter().enumerate() {
                assert_eq!(
                    entry, &submitted[index],
                    "case {label:?}: container {index} must read back as it was declared, got \
                     {entry:?}"
                );
                for named in ["name", "image"] {
                    assert!(
                        entry[named].as_str().is_some_and(|value| !value.is_empty()),
                        "case {label:?}: container {index} must name its {named}, got {entry:?}"
                    );
                }
            }
        }

        // ROUND TRIP: it reads back at its own address as the object the create
        // reported. The WHOLE object is compared, which is stable here because this
        // cluster has no node group: no pod of it is bound to a node, so the clock
        // owes none of them a move between the two reads.
        let read_back = dial(&config, label, "GET", case.object_path, None).await;
        assert_eq!(
            read_back.status, 200,
            "case {label:?}: the object reads back at {:?}, got {read_back:?}",
            case.object_path
        );
        assert_eq!(
            read_back.body, created.body,
            "case {label:?}: the read must answer the object the create answered, got {:?} \
             after {:?}",
            read_back.body, created.body
        );
    }
}
