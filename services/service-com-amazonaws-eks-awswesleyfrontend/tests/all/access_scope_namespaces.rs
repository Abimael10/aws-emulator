// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the `AccessScope` this service SERVES on an access-policy
//! association, through both operations that carry one: `AssociateAccessPolicy`'s
//! own answer and `ListAssociatedAccessPolicies`.
//!
//! RULE: a served `AccessScope` carries BOTH of its modelled members — the scope
//! `type` and the `namespaces` list — with the list present and empty when the
//! scope covers no single namespace (a `cluster`-scoped association) and carrying
//! the names the association was scoped with, in order, when it is
//! `namespace`-scoped. A reader asking an association which namespaces it covers
//! gets a list to iterate either way, never a missing member.
//!
//! The rows span what the rule generalises over: the two `AccessScopeType`
//! members, how the request expresses "no namespaces" (the member absent, or sent
//! as an empty list), the NUMBER of namespaces, their ORDER, and the longest name
//! Kubernetes accepts — plus the association `CreateCluster` materialises for the
//! cluster creator, which no `AssociateAccessPolicy` call wrote.
//!
//! The wire harness is the crate's one copy in `tests/all/wire.rs` and the cluster
//! bodies come from `cluster_lifecycle`: no suite grows its own client.

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    access_policy_arn, ACCESS_POLICY_NAMES, ACCESS_SCOPE_TYPE_CLUSTER,
    ACCESS_SCOPE_TYPE_NAMESPACE, CLUSTER_ADMIN_ACCESS_POLICY_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{access_entry_cluster_request, minimal_request, REGION};
use crate::wire::{call, emulator, encode_label, Answer};

/// One row: how the request spells the scope, and the `namespaces` list the answer
/// must carry for it.
struct Case {
    label: &'static str,
    /// The access policy this row associates. Each row uses its OWN policy, since
    /// an association is keyed by policy ARN, so all rows accumulate on one entry.
    policy: &'static str,
    requested_scope: serde_json::Value,
    expected_namespaces: serde_json::Value,
}

/// The longest name a Kubernetes namespace can carry (a DNS label), so a row
/// exercises the boundary of what a scoped name can be.
const LONGEST_NAMESPACE_NAME_CHARS: usize = 63;

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "cluster scope, no namespaces member in the request",
            policy: CLUSTER_ADMIN_ACCESS_POLICY_NAME,
            requested_scope: serde_json::json!({ "type": ACCESS_SCOPE_TYPE_CLUSTER }),
            expected_namespaces: serde_json::json!([]),
        },
        Case {
            label: "cluster scope, an explicitly empty namespaces list in the request",
            policy: "AmazonEKSAdminPolicy",
            requested_scope: serde_json::json!({
                "type": ACCESS_SCOPE_TYPE_CLUSTER,
                "namespaces": [],
            }),
            expected_namespaces: serde_json::json!([]),
        },
        Case {
            label: "namespace scope, one namespace",
            policy: "AmazonEKSViewPolicy",
            requested_scope: serde_json::json!({
                "type": ACCESS_SCOPE_TYPE_NAMESPACE,
                "namespaces": ["kube-system"],
            }),
            expected_namespaces: serde_json::json!(["kube-system"]),
        },
        Case {
            label: "namespace scope, three namespaces in a non-sorted order",
            policy: "AmazonEKSEditPolicy",
            requested_scope: serde_json::json!({
                "type": ACCESS_SCOPE_TYPE_NAMESPACE,
                "namespaces": ["zeta", "alpha", "mid"],
            }),
            expected_namespaces: serde_json::json!(["zeta", "alpha", "mid"]),
        },
        Case {
            label: "namespace scope, the longest namespace name Kubernetes accepts",
            policy: "AmazonEKSAdminViewPolicy",
            requested_scope: serde_json::json!({
                "type": ACCESS_SCOPE_TYPE_NAMESPACE,
                "namespaces": ["n".repeat(LONGEST_NAMESPACE_NAME_CHARS)],
            }),
            expected_namespaces: serde_json::json!(["n".repeat(LONGEST_NAMESPACE_NAME_CHARS)]),
        },
    ]
}

/// The `accessScope` an answer must carry for a row: the requested type, and the
/// namespaces list the rule says is always present.
fn expected_scope(case: &Case) -> serde_json::Value {
    serde_json::json!({
        "type": case.requested_scope["type"].clone(),
        "namespaces": case.expected_namespaces.clone(),
    })
}

const ACCOUNT: &str = "000000000000";

fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

async fn create_cluster(router: &MultiServiceRouter, body: &serde_json::Value) -> Answer {
    call(router, REGION, "POST", "/clusters", &body.to_string()).await
}

fn entry_path(cluster: &str, principal: &str) -> String {
    format!(
        "/clusters/{}/access-entries/{}",
        encode_label(cluster),
        encode_label(principal)
    )
}

async fn associate(
    router: &MultiServiceRouter,
    cluster: &str,
    principal: &str,
    policy_arn: &str,
    access_scope: &serde_json::Value,
) -> Answer {
    let body = serde_json::json!({ "policyArn": policy_arn, "accessScope": access_scope });
    call(
        router,
        REGION,
        "POST",
        &format!("{}/access-policies", entry_path(cluster, principal)),
        &body.to_string(),
    )
    .await
}

async fn list_associated(router: &MultiServiceRouter, cluster: &str, principal: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("{}/access-policies", entry_path(cluster, principal)),
        "",
    )
    .await
}

/// The associations a `ListAssociatedAccessPolicies` answer reports, in order.
fn listed_associations(answer: &Answer) -> Vec<serde_json::Value> {
    answer.body["associatedAccessPolicies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A cluster that serves the access-entry API with one `STANDARD` access entry on
/// it, ready to associate policies to. Returns the principal ARN the entry is
/// keyed by.
async fn cluster_with_entry(router: &MultiServiceRouter, cluster: &str, role: &str) -> String {
    let created = create_cluster(router, &access_entry_cluster_request(cluster)).await;
    assert_eq!(
        created.status, 200,
        "setup: creating cluster {cluster:?} must succeed, got {created:?}"
    );
    let principal = role_arn(role);
    let entry = call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/access-entries", encode_label(cluster)),
        &serde_json::json!({ "principalArn": principal }).to_string(),
    )
    .await;
    assert_eq!(
        entry.status, 200,
        "setup: creating the access entry for {principal:?} must succeed, got {entry:?}"
    );
    principal
}

#[tokio::test]
async fn every_served_access_scope_carries_a_namespaces_list() {
    let router = emulator();
    let cluster = "scope-namespaces";
    let principal = cluster_with_entry(&router, cluster, "scope-namespaces-role").await;

    let cases = cases();
    for case in &cases {
        // Each row names a policy from the catalogue a caller discovers through
        // `ListAccessPolicies` ([`ACCESS_POLICY_NAMES`]), which is the only set
        // this service lets a caller associate.
        assert!(
            ACCESS_POLICY_NAMES.contains(&case.policy),
            "case {:?}: {:?} must be a policy the catalogue publishes",
            case.label,
            case.policy
        );
        let policy_arn = access_policy_arn(case.policy);

        let answered = associate(
            &router,
            cluster,
            &principal,
            &policy_arn,
            &case.requested_scope,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected HTTP 200, got {answered:?}",
            case.label
        );
        let written = answered.body["associatedAccessPolicy"]["accessScope"].clone();
        assert_eq!(
            written,
            expected_scope(case),
            "case {:?}: expected the write to answer accessScope {}, got {written}",
            case.label,
            expected_scope(case)
        );

        // The read-back: the listing reports the same scope, namespaces member and
        // all.
        let listed = list_associated(&router, cluster, &principal).await;
        assert_eq!(
            listed.status, 200,
            "case {:?}: the listing must succeed, got {listed:?}",
            case.label
        );
        let found = listed_associations(&listed)
            .into_iter()
            .find(|item| item["policyArn"].as_str() == Some(policy_arn.as_str()));
        let read = found
            .map(|item| item["accessScope"].clone())
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            read,
            expected_scope(case),
            "case {:?}: expected the listing to report accessScope {}, got {read}",
            case.label,
            expected_scope(case)
        );
    }

    // Every row accumulated on the one entry, and NONE of them is missing the
    // list — whatever the scope of its neighbours.
    let listed = list_associated(&router, cluster, &principal).await;
    let associations = listed_associations(&listed);
    assert_eq!(
        associations.len(),
        cases.len(),
        "the entry must hold one association per row, got {listed:?}"
    );
    for association in &associations {
        assert!(
            association["accessScope"]["namespaces"].is_array(),
            "every listed association must carry a namespaces list, got {association}"
        );
    }
}

/// The same rule holds for the association no `AssociateAccessPolicy` call wrote:
/// the cluster-scoped cluster-admin association `CreateCluster` materialises for
/// the creating principal.
#[tokio::test]
async fn the_cluster_creators_bootstrap_association_carries_a_namespaces_list() {
    let router = emulator();
    // The creator is the IAM principal the request's own credential speaks for,
    // read from the one definition of it rather than written out again.
    let principal = ::service_common::caller_principal::ambient_user_arn();

    // The rows differ in how the create asks for the bootstrap association: the
    // flag left to its default (`true`) and the flag set explicitly.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "the bootstrap flag left to its default",
            serde_json::json!({ "authenticationMode": "API_AND_CONFIG_MAP" }),
        ),
        (
            "the bootstrap flag set explicitly",
            serde_json::json!({
                "authenticationMode": "API_AND_CONFIG_MAP",
                "bootstrapClusterCreatorAdminPermissions": true,
            }),
        ),
    ];

    for (index, (case, access_config)) in cases.iter().enumerate() {
        let cluster = format!("bootstrap-scope-{index}");
        let mut body = minimal_request(&cluster);
        body["accessConfig"] = access_config.clone();
        let created = create_cluster(&router, &body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the cluster must succeed, got {created:?}"
        );

        let listed = list_associated(&router, &cluster, &principal).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: the creator's associations must be readable, got {listed:?}"
        );
        let scopes: Vec<serde_json::Value> = listed_associations(&listed)
            .into_iter()
            .filter(|item| {
                item["policyArn"].as_str()
                    == Some(access_policy_arn(CLUSTER_ADMIN_ACCESS_POLICY_NAME).as_str())
            })
            .map(|item| item["accessScope"].clone())
            .collect();
        assert_eq!(
            scopes,
            vec![serde_json::json!({
                "type": ACCESS_SCOPE_TYPE_CLUSTER,
                "namespaces": [],
            })],
            "case {case:?}: expected one cluster-scoped bootstrap association carrying an \
             empty namespaces list, got {listed:?}"
        );
    }
}
