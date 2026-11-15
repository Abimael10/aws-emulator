// SPDX-License-Identifier: CC0-1.0

//! `Cluster$vendedLogs`: the member every cluster answer carries.
//!
//! The RULE under test: every operation that answers with a `Cluster` reports
//! `vendedLogs` on it as a BOOLEAN — never an absent member — and a cluster for which
//! no vended log delivery is configured reports it `false`. Nothing in this API
//! configures such a delivery: no member of any request names one and no operation
//! writes one, so the member reads the same way whatever the request asked for,
//! whichever status the cluster is in, and whichever of the five `Cluster`-answering
//! operations answers it (`CreateCluster`, `DescribeCluster`, `DeleteCluster`,
//! `RegisterCluster`, `DeregisterCluster`).
//!
//! Two suites assert it, each over a TABLE whose rows differ along what the rule
//! generalises over:
//!
//! * the Amazon EKS clusters `CreateCluster` provisions — rows differing in which
//!   optional members the request names (none, the control-plane logging that is the
//!   member's nearest neighbour, tags, an encryption key, the compute and storage
//!   capabilities, deletion protection) — each read through the create's own answer,
//!   through `DescribeCluster` while the cluster is still `CREATING`, through
//!   `DescribeCluster` once the clock owes it `ACTIVE`, and through the answer
//!   `DeleteCluster` gives;
//! * the external clusters `RegisterCluster` connects, which carry no Amazon EKS
//!   control plane at all — rows differing in the provider named and in whether tags
//!   are supplied — read through the registration's answer, through `DescribeCluster`
//!   and through the answer `DeregisterCluster` gives.
//!
//! The wire harness is the crate's own (`wire.rs`) and the per-operation helpers are
//! the ones `cluster_lifecycle` owns; nothing is re-implemented here.

use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{create, delete, describe, minimal_request, REGION};
use crate::wire::{call, emulator, encode_label, read_until, Answer};

/// What every cluster answer must carry for a cluster with no vended log delivery.
fn assert_vended_logs(case: &str, answered_by: &str, cluster: &serde_json::Value) {
    let reported = cluster.get("vendedLogs");
    assert_eq!(
        reported,
        Some(&serde_json::Value::Bool(false)),
        "case {case:?} answered by {answered_by}: expected vendedLogs reported as the \
         boolean false, got {reported:?} on {cluster:?}"
    );
}

/// `DELETE /cluster-registrations/{name}`: the deregistration of a connected cluster,
/// which answers with the `Cluster` it removed.
async fn deregister(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        REGION,
        "DELETE",
        &format!("/cluster-registrations/{}", encode_label(name)),
        "",
    )
    .await
}

/// `POST /cluster-registrations`: the registration of an external cluster.
async fn register(router: &MultiServiceRouter, body: &serde_json::Value) -> Answer {
    call(
        router,
        REGION,
        "POST",
        "/cluster-registrations",
        &body.to_string(),
    )
    .await
}

#[tokio::test]
async fn every_amazon_eks_cluster_answer_reports_vended_logs() {
    let router = emulator();

    // The rows differ in which optional members the request names — and in
    // particular in the control-plane logging the cluster runs with, the member a
    // caller could mistake this one for: a cluster that exports every log type
    // reports `vendedLogs` exactly as one that exports none.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("only the required members", minimal_request("vl-minimal")),
        (
            "every control plane log type exported",
            serde_json::json!({
                "name": "vl-all-logs",
                "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
                "resourcesVpcConfig": { "subnetIds": ["subnet-11111111", "subnet-22222222"] },
                "logging": {
                    "clusterLogging": [{
                        "types": [
                            "api",
                            "audit",
                            "authenticator",
                            "controllerManager",
                            "scheduler",
                        ],
                        "enabled": true,
                    }],
                },
            }),
        ),
        (
            "control plane logging explicitly off",
            serde_json::json!({
                "name": "vl-logging-off",
                "roleArn": "arn:aws:iam::000000000000:role/eksClusterRole",
                "resourcesVpcConfig": { "subnetIds": ["subnet-11111111", "subnet-22222222"] },
                "logging": { "clusterLogging": [{ "types": ["api"], "enabled": false }] },
            }),
        ),
        (
            "tags, an encryption key and the capability members",
            serde_json::json!({
                "name": "vl-fully-specified",
                "roleArn": "arn:aws:iam::000000000000:role/other",
                "resourcesVpcConfig": {
                    "subnetIds": ["subnet-11111111", "subnet-22222222"],
                    "endpointPublicAccess": false,
                    "endpointPrivateAccess": true,
                },
                "kubernetesNetworkConfig": { "ipFamily": "ipv6" },
                "logging": { "clusterLogging": [{ "types": ["audit"], "enabled": true }] },
                "tags": { "env": "dev", "team": "platform" },
                "encryptionConfig": [{
                    "resources": ["secrets"],
                    "provider": { "keyArn": "arn:aws:kms:us-west-2:000000000000:key/abcd" },
                }],
                "computeConfig": { "enabled": false },
                "storageConfig": { "blockStorage": { "enabled": false } },
                "deletionProtection": false,
            }),
        ),
    ];

    for (case, request) in &cases {
        let name = request["name"].as_str().expect("the case names its cluster");

        let created = create(&router, request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateCluster, got {created:?}"
        );
        assert_vended_logs(case, "CreateCluster", created.cluster());

        // While the cluster is still CREATING: the member is a property of the
        // cluster, not of a status it has reached.
        let creating = describe(&router, name).await;
        assert_eq!(
            creating.status, 200,
            "case {case:?}: expected 200 from DescribeCluster, got {creating:?}"
        );
        assert_vended_logs(case, "DescribeCluster while CREATING", creating.cluster());

        // And once the clock owes the cluster ACTIVE, which is what provisions the
        // control plane behind it.
        let active = read_until(
            case,
            "the clock owes a new cluster ACTIVE",
            || describe(&router, name),
            |answer| answer.cluster()["status"] == serde_json::json!("ACTIVE"),
        )
        .await;
        assert_vended_logs(case, "DescribeCluster once ACTIVE", active.cluster());

        // The modelled members the create asked for are still reported beside it,
        // so the extra member displaces none of them.
        assert_eq!(
            active.cluster()["name"],
            serde_json::json!(name),
            "case {case:?}: expected the cluster's own name beside vendedLogs, got {:?}",
            active.cluster()["name"]
        );
        assert_eq!(
            active.cluster()["logging"],
            creating.cluster()["logging"],
            "case {case:?}: expected the logging configuration unchanged beside \
             vendedLogs, got {:?}",
            active.cluster()["logging"]
        );

        let deleted = delete(&router, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected 200 from DeleteCluster, got {deleted:?}"
        );
        assert_eq!(
            deleted.cluster()["status"],
            serde_json::json!("DELETING"),
            "case {case:?}: a deleted cluster is DELETING, got {:?}",
            deleted.cluster()["status"]
        );
        assert_vended_logs(case, "DeleteCluster", deleted.cluster());
    }
}

#[tokio::test]
async fn every_connected_cluster_answer_reports_vended_logs() {
    let router = emulator();

    // A connected cluster runs outside Amazon Web Services, so it carries no
    // control-plane logging configuration at all — and still reports the member.
    // The providers are members of `ConnectorConfigProvider`
    // (`smithy_query(query="shape", shape="ConnectorConfigProvider")`).
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "only the required members",
            serde_json::json!({
                "name": "vl-connected-minimal",
                "connectorConfig": {
                    "roleArn": "arn:aws:iam::000000000000:role/AmazonEKSConnectorRole",
                    "provider": "OTHER",
                },
            }),
        ),
        (
            "another provider, with tags",
            serde_json::json!({
                "name": "vl-connected-tagged",
                "connectorConfig": {
                    "roleArn": "arn:aws:iam::000000000000:role/AmazonEKSConnectorRole",
                    "provider": "EKS_ANYWHERE",
                },
                "tags": { "env": "dev" },
            }),
        ),
    ];

    for (case, request) in &cases {
        let name = request["name"].as_str().expect("the case names its cluster");

        let registered = register(&router, request).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: expected 200 from RegisterCluster, got {registered:?}"
        );
        assert_vended_logs(case, "RegisterCluster", registered.cluster());

        let described = describe(&router, name).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: expected 200 from DescribeCluster, got {described:?}"
        );
        assert_vended_logs(case, "DescribeCluster", described.cluster());
        assert!(
            described.cluster()["logging"].is_null(),
            "case {case:?}: a connected cluster carries no control plane logging, got \
             {:?}",
            described.cluster()["logging"]
        );

        let deregistered = deregister(&router, name).await;
        assert_eq!(
            deregistered.status, 200,
            "case {case:?}: expected 200 from DeregisterCluster, got {deregistered:?}"
        );
        assert_vended_logs(case, "DeregisterCluster", deregistered.cluster());
    }
}
