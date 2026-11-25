// SPDX-License-Identifier: CC0-1.0

//! The certificate authority a cluster PUBLISHES is the trust anchor of the
//! certificate served at THAT cluster's own published endpoint.
//!
//! The RULE under test: **`Cluster$endpoint` is "the endpoint for your Kubernetes
//! API server" and `Certificate$data` is "the Base64-encoded certificate data
//! required to communicate with your cluster. Add this to the
//! `certificate-authority-data` section of the kubeconfig file for your cluster"
//! (`smithy_query(query="shape", shape="Certificate")`) — so Base64-decoding that
//! member yields the certificate authority that SIGNED the certificate the cluster's
//! endpoint presents: a client that pins exactly that bundle and nothing else
//! completes the handshake and is served the Kubernetes API, and one that pins the
//! bundle ANOTHER cluster published is refused in the TLS layer before any request
//! is served. The material belongs to the cluster that published it rather than to
//! the service.**
//!
//! The rows differ along what the rule generalises over: which cluster's published
//! authority is pinned against which cluster's published endpoint (every pairing of
//! three clusters, two in one region and one in another, so the diagonal is served
//! and every other pairing is refused), and — over the client that pinned the right
//! one — the Kubernetes paths such a client calls on a cluster whose ONLY capacity is
//! an ACTIVE Fargate profile: the namespace collection read and written, and the pod
//! collection of a namespace that profile selects, with each object read back.
//!
//! The client is the raw, kubeconfig-built one `kubernetes_api.rs` dials with — a TCP
//! connection to the published address, TLS verified against nothing but the pinned
//! authority, and one HTTP/1.1 exchange. The pairings that must be REFUSED are opened
//! through that module's own fallible connect
//! (`crate::kubernetes_api::tls_connection`), which answers the handshake error
//! instead of panicking, so both clients are built by the one place that builds them.

use service_com_amazonaws_eks_awswesleyfrontend::state::kubernetes_fargate::fargate_node_name;

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, provisioned_cluster, tls_connection, Kubeconfig};
use crate::kubernetes_fargate_scheduling::active_profile;
use crate::wire::emulator;

/// The second region a row is created in, so the rule is shown to hold per cluster
/// rather than per region.
const OTHER_REGION: &str = "eu-central-1";

/// The namespace the Fargate profile of this suite selects, which is the one a
/// kubeconfig-built client creates its pod in.
const SELECTED_NAMESPACE: &str = "default";

/// Open a TLS connection to `endpoint` trusting NOTHING but the certificates
/// `trusted_pem` carries, and answer what the handshake did: `Ok` for a handshake the
/// served certificate verified in, and the error it was refused with otherwise.
///
/// The connection is the crate's own ([`tls_connection`]); what this adds is that the
/// stream is dropped straight away, because what a row asserts is the handshake rather
/// than anything served over it.
async fn handshake(endpoint: &str, trusted_pem: &str) -> Result<(), String> {
    tls_connection(endpoint, trusted_pem)
        .await
        .map(|_connection| ())
}

// ---------------------------------------------------------------------------
// RULE: the authority a cluster published signs the certificate served at that
// cluster's endpoint, and no other cluster's.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_clusters_published_authority_is_the_trust_anchor_of_its_own_endpoint() {
    let router = emulator();

    // Three clusters, two in one region and one in another: the material is per
    // cluster, so every pairing of a published authority with a published endpoint is
    // asked.
    let clusters = [
        ("the first cluster", REGION, "kubeconfig-trust-one"),
        ("a second cluster in the same region", REGION, "kubeconfig-trust-two"),
        ("a cluster in another region", OTHER_REGION, "kubeconfig-trust-one"),
    ];
    let mut published: Vec<(&str, Kubeconfig)> = Vec::new();
    for (case, region, name) in clusters {
        published.push((case, provisioned_cluster(&router, region, name).await));
    }

    for (dialled_index, (dialled_case, dialled)) in published.iter().enumerate() {
        for (pinned_index, (pinned_case, pinned)) in published.iter().enumerate() {
            let own = dialled_index == pinned_index;
            let answered = handshake(&dialled.endpoint, &pinned.certificate_authority_pem).await;
            let case = format!("{dialled_case} dialled trusting {pinned_case}");
            if own {
                assert!(
                    answered.is_ok(),
                    "case {case:?}: the certificate served at the cluster's own endpoint must \
                     verify against the authority it published, got {answered:?}"
                );
                continue;
            }
            assert!(
                answered.is_err(),
                "case {case:?}: a client pinning another cluster's published authority must be \
                 refused in the TLS layer, got {answered:?}"
            );
        }
    }

    // Two reads of one cluster publish the same authority, and no two clusters publish
    // the same one: the handshake above therefore turns on the material rather than on
    // one bundle the service hands every cluster.
    for (index, (case, config)) in published.iter().enumerate() {
        assert!(
            config
                .certificate_authority_pem
                .starts_with("-----BEGIN CERTIFICATE-----"),
            "case {case:?}: the decoded authority must be a PEM certificate block, got {:?}",
            config.certificate_authority_pem
        );
        let others = published
            .iter()
            .enumerate()
            .filter(|(other_index, _)| *other_index != index)
            .filter(|(_, (_, other))| {
                other.certificate_authority_pem == config.certificate_authority_pem
            })
            .count();
        assert_eq!(
            others, 0,
            "case {case:?}: each cluster publishes an authority of its own, but one was \
             published twice"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: what answers at the published endpoint, to a client that pinned the
// published authority, is the Kubernetes API — on a cluster whose only capacity is
// an ACTIVE Fargate profile.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_kubeconfig_built_client_is_served_the_kubernetes_api_of_a_fargate_cluster() {
    let router = emulator();
    let cluster = "kubeconfig-fargate-api";
    let config = provisioned_cluster(&router, REGION, cluster).await;
    // Its ONLY capacity: one Fargate profile selecting the namespace the pod below is
    // created in, created and waited for through the crate's own driver.
    active_profile(
        &router,
        cluster,
        "kubeconfig-profile",
        &serde_json::json!([{ "namespace": SELECTED_NAMESPACE }]),
    )
    .await;

    // The rows are the requests such a client makes: the namespace collection read and
    // written, and the pod collection of the namespace the profile selects.
    let pod = "kubeconfig-pod";
    let namespace = "aws-observability";
    let pods_path = format!("/api/v1/namespaces/{SELECTED_NAMESPACE}/pods");
    let rows: [(&str, &str, &str, Option<serde_json::Value>, u16); 3] = [
        (
            "the namespace collection, read",
            "GET",
            "/api/v1/namespaces",
            None,
            200,
        ),
        (
            "a namespace, created",
            "POST",
            "/api/v1/namespaces",
            Some(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": { "name": namespace },
            })),
            201,
        ),
        (
            "a pod of the namespace the profile selects, created",
            "POST",
            pods_path.as_str(),
            Some(serde_json::json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": { "name": pod },
                "spec": {
                    "containers": [{
                        "name": "app",
                        "image": "public.ecr.aws/nginx/nginx:1.27",
                    }],
                },
            })),
            201,
        ),
    ];
    for (case, method, path, body, expected) in &rows {
        let answered = dial(&config, case, method, path, body.as_ref()).await;
        assert_eq!(
            answered.status, *expected,
            "case {case:?}: expected HTTP {expected} from the published endpoint, got {answered:?}"
        );
    }

    // ROUND TRIP: each object the client created is read back through the collection
    // and by name, over the same verified connection.
    let listed = dial(
        &config,
        "the namespaces after the create",
        "GET",
        "/api/v1/namespaces",
        None,
    )
    .await;
    let names: Vec<String> = listed.body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["metadata"]["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|held| held == namespace),
        "case \"the namespaces after the create\": the namespace the client created must be \
         listed, got {names:?}"
    );

    let read = dial(
        &config,
        "the pod, read back",
        "GET",
        &format!("/api/v1/namespaces/{SELECTED_NAMESPACE}/pods/{pod}"),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case \"the pod, read back\": the pod must be readable, got {read:?}"
    );
    assert_eq!(
        read.body["spec"]["containers"][0]["image"],
        serde_json::json!("public.ecr.aws/nginx/nginx:1.27"),
        "case \"the pod, read back\": the spec the cluster admitted is the one it reports, got \
         {read:?}"
    );
    // The capacity it was placed on is the Fargate profile's, which is the only
    // capacity this cluster has.
    assert_eq!(
        read.body["spec"]["nodeName"],
        serde_json::json!(fargate_node_name(SELECTED_NAMESPACE, pod)),
        "case \"the pod, read back\": the pod must be placed on the profile's own capacity, got \
         {read:?}"
    );
}
