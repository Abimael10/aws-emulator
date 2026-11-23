// SPDX-License-Identifier: CC0-1.0

//! WHETHER the IAM verdict on the role an EKS Pod Identity association mutation carries can
//! change the answer a cluster that does not resolve earns.
//!
//! RULE: `CreatePodIdentityAssociation` and `UpdatePodIdentityAssociation` resolve the
//! CLUSTER their `@httpLabel` names before the role the body carries is resolved against
//! IAM, so a cluster this service holds no record of is answered with the 404
//! `ResourceNotFoundException` that names only that cluster
//! ([`service_com_amazonaws_eks_awswesleyfrontend::state::cluster_not_found_message`]) — the
//! SAME answer whatever IAM would say about the role, because the role is never examined for
//! an association there is no cluster to write it under.
//!
//! `pod_identity_resolves_the_cluster_first` pins the order for the role classes IAM would
//! REFUSE. What this suite adds is the other side of the invariance, which is what shows the
//! role is not examined at all rather than examined and forgiven:
//!
//! * the role whose trust policy ADMITS the EKS Pod Identity principal — the one a create on
//!   a cluster that does resolve is written for — earns the identical 404 on a cluster that
//!   does not, so the three IAM verdicts (no such role, a trust policy that admits nothing, a
//!   trust policy that admits the principal) are ONE answer;
//! * the mutations' optional members — a target role, session tags, tags, and on the update a
//!   body that names no role at all — do not change it either: the update's own association
//!   not-found never precedes the cluster's;
//! * nothing is written along the way: the cluster that DOES resolve still lists no
//!   association after every refused row, and the admitting role then creates one there and
//!   reads back, which is what proves the refusals above were the cluster's and not the
//!   role's.
//!
//! The rows span the three IAM verdicts, the ways a cluster name fails to resolve (never
//! created, differing only in case, both documented bounds of `clusterName`'s `length
//! 1..100`, and the real name signed for another Region) and the optional members each
//! mutation may carry.
//!
//! IAM is mounted beside this service (`crate::wire::emulator_with_identity`), because a
//! verdict about a trust policy needs an IAM that holds one.

use serde_json::json;
use service_com_amazonaws_eks_awswesleyfrontend::state::{
    cluster_not_found_message, MAX_CLUSTER_NAME_LEN, POD_IDENTITY_SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::pod_identity_resolves_the_cluster_first::{
    create_role, role_arn, service_trust_policy,
};
use crate::wire::{call, emulator_with_identity, encode_label, error_code, Answer};

/// The Region every call is signed for, and the one the fixture cluster lives in.
const REGION: &str = "us-west-2";

/// Another Region, where the fixture cluster does not exist — a cluster is resolved per
/// Region, so the same name signed for this one resolves to nothing.
const OTHER_REGION: &str = "eu-west-1";

/// The cluster that exists: the control the refused rows are measured against.
const CLUSTER: &str = "podid-verdict-real";

/// The Kubernetes namespace every row names: a well-formed name, so no row is refused for
/// its body.
const NAMESPACE: &str = "default";

/// A role IAM holds whose trust policy admits the EKS Pod Identity principal — the verdict
/// under which a create SUCCEEDS, and which must still earn the cluster's 404.
const ADMITTING_ROLE: &str = "podIdentityVerdictAdmittingRole";

/// A role IAM holds whose trust policy admits another service principal only, so the trust
/// question has a grounded refusal to answer with.
const WITHHOLDING_ROLE: &str = "podIdentityVerdictWithholdingRole";

/// A role of this account IAM holds no record of at all.
const ABSENT_ROLE: &str = "podIdentityVerdictNoSuchRole";

/// The emulator the rows drive: this service beside IAM, with one cluster that exists and
/// the two roles IAM holds.
async fn fixture() -> MultiServiceRouter {
    let router = emulator_with_identity();
    let created = call(
        &router,
        REGION,
        "POST",
        "/clusters",
        &json!({
            "name": CLUSTER,
            "roleArn": role_arn("eksClusterRole"),
            "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the fixture cluster must be created, got {created:?}"
    );
    create_role(
        &router,
        ADMITTING_ROLE,
        service_trust_policy(POD_IDENTITY_SERVICE_PRINCIPAL),
    )
    .await;
    create_role(
        &router,
        WITHHOLDING_ROLE,
        service_trust_policy("ec2.amazonaws.com"),
    )
    .await;
    router
}

/// The three IAM verdicts a well-formed role ARN of this account can earn: `(why, role
/// name)`. Every one of them is answered the same way on a cluster that does not resolve.
fn iam_verdicts() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "a role whose trust policy admits the pod identity principal",
            ADMITTING_ROLE,
        ),
        (
            "a role whose trust policy admits another principal",
            WITHHOLDING_ROLE,
        ),
        ("a role IAM holds no record of", ABSENT_ROLE),
    ]
}

/// The cluster names no row can resolve, and why each one cannot: `(why, region, cluster)`.
fn unresolvable_clusters() -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "a cluster that was never created",
            REGION,
            "podid-verdict-ghost".to_string(),
        ),
        (
            "a cluster name differing from the real one only in case",
            REGION,
            CLUSTER.to_ascii_uppercase(),
        ),
        (
            "a cluster name at the documented length floor",
            REGION,
            "p".to_string(),
        ),
        (
            "a cluster name at the documented length ceiling",
            REGION,
            "p".repeat(MAX_CLUSTER_NAME_LEN),
        ),
        (
            "the real cluster name, signed for another Region",
            OTHER_REGION,
            CLUSTER.to_string(),
        ),
    ]
}

/// `CreatePodIdentityAssociation` with `body` under `cluster`, signed for `region`.
async fn create_association(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    body: serde_json::Value,
) -> Answer {
    call(
        router,
        region,
        "POST",
        &format!(
            "/clusters/{}/pod-identity-associations",
            encode_label(cluster)
        ),
        &body.to_string(),
    )
    .await
}

/// `UpdatePodIdentityAssociation` of `association_id` with `body` under `cluster`.
async fn update_association(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    association_id: &str,
    body: serde_json::Value,
) -> Answer {
    call(
        router,
        region,
        "POST",
        &format!(
            "/clusters/{}/pod-identity-associations/{}",
            encode_label(cluster),
            encode_label(association_id)
        ),
        &body.to_string(),
    )
    .await
}

/// `ListPodIdentityAssociations` of `cluster`, as the control that nothing was written.
async fn list_associations(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/pod-identity-associations",
            encode_label(cluster)
        ),
        "",
    )
    .await
}

/// What a client branches on, reduced to the tuple the rule fixes: the status, the error
/// class, the cluster the payload names and the message. Two rows that answer the same
/// tuple answered IDENTICALLY, which is what "the role is never examined" means.
fn refusal(answer: &Answer) -> (u16, String, Option<String>, String) {
    (
        answer.status,
        error_code(answer),
        answer.body["clusterName"].as_str().map(str::to_owned),
        answer.message(),
    )
}

/// The tuple a cluster that does not resolve must answer with, whatever else the request
/// carries.
fn cluster_not_found(cluster: &str) -> (u16, String, Option<String>, String) {
    (
        404,
        "ResourceNotFoundException".to_string(),
        Some(cluster.to_string()),
        cluster_not_found_message(cluster),
    )
}

// ---------------------------------------------------------------------------
// RULE: one answer per unresolvable cluster, the same one for every IAM verdict — the
// admitting role included.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_create_on_an_unresolvable_cluster_answers_the_same_whatever_iam_says_about_the_role()
{
    let router = fixture().await;

    for (why_cluster, region, cluster) in unresolvable_clusters() {
        let expected = cluster_not_found(&cluster);
        for (why_role, role) in iam_verdicts() {
            let case = format!("CreatePodIdentityAssociation / {why_role} / {why_cluster}");
            let answer = create_association(
                &router,
                region,
                &cluster,
                json!({
                    "namespace": NAMESPACE,
                    "serviceAccount": "podid-verdict-create",
                    "roleArn": role_arn(role),
                }),
            )
            .await;
            assert_eq!(
                refusal(&answer),
                expected,
                "case {case}: the unresolvable cluster must be reported before the role is \
                 resolved against IAM, got {answer:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_on_an_unresolvable_cluster_answers_the_same_whatever_iam_says_about_the_role()
{
    let router = fixture().await;

    // A well-formed association id this emulator never minted: the update's own not-found is
    // about the association, and it comes AFTER the cluster's.
    let association_id = "a-00000000000000000";
    for (why_cluster, region, cluster) in unresolvable_clusters() {
        let expected = cluster_not_found(&cluster);
        for (why_role, role) in iam_verdicts() {
            let case = format!("UpdatePodIdentityAssociation / {why_role} / {why_cluster}");
            let answer = update_association(
                &router,
                region,
                &cluster,
                association_id,
                json!({ "roleArn": role_arn(role) }),
            )
            .await;
            assert_eq!(
                refusal(&answer),
                expected,
                "case {case}: the unresolvable cluster must be reported before the role is \
                 resolved against IAM, got {answer:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE, over the optional members each mutation may carry: what else the body says does not
// change the answer either — including an update that names no role at all, whose only other
// candidate refusal is the association's own not-found.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_cluster_is_reported_whatever_optional_members_the_mutation_carries() {
    let router = fixture().await;

    let cluster = "podid-verdict-ghost-members";
    let expected = cluster_not_found(cluster);
    let admitting = role_arn(ADMITTING_ROLE);
    let withholding = role_arn(WITHHOLDING_ROLE);

    let create_bodies = vec![
        (
            "a create carrying a target role and disabled session tags",
            json!({
                "namespace": NAMESPACE,
                "serviceAccount": "podid-verdict-target",
                "roleArn": admitting,
                "targetRoleArn": role_arn("podIdentityVerdictTargetRole"),
                "disableSessionTags": true,
            }),
        ),
        (
            "a create carrying tags and a client request token",
            json!({
                "namespace": NAMESPACE,
                "serviceAccount": "podid-verdict-tagged",
                "roleArn": withholding,
                "tags": { "owner": "podid-verdict" },
                "clientRequestToken": "podid-verdict-token",
            }),
        ),
    ];
    for (why, body) in create_bodies {
        let answer = create_association(&router, REGION, cluster, body).await;
        assert_eq!(
            refusal(&answer),
            expected,
            "case {why}: the unresolvable cluster must be reported whatever else the body \
             carries, got {answer:?}"
        );
    }

    let update_bodies = vec![
        (
            "an update that changes the role",
            json!({ "roleArn": admitting }),
        ),
        (
            "an update that changes only the target role",
            json!({ "targetRoleArn": role_arn("podIdentityVerdictTargetRole") }),
        ),
        (
            "an update that changes only the session tags, naming no role",
            json!({ "disableSessionTags": true }),
        ),
    ];
    for (why, body) in update_bodies {
        let answer =
            update_association(&router, REGION, cluster, "a-00000000000000000", body).await;
        assert_eq!(
            refusal(&answer),
            expected,
            "case {why}: the unresolvable cluster must be reported before the association \
             it would hold, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// CONTROL: nothing was written by the refused rows, and the admitting role — refused above
// only because of the cluster — creates an association on the cluster that resolves and
// reads back. That is what shows those 404s were the cluster's and not the role's.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_admitting_role_refused_on_a_ghost_cluster_still_creates_on_the_real_one() {
    let router = fixture().await;

    let ghost = "podid-verdict-ghost-control";
    let refused = create_association(
        &router,
        REGION,
        ghost,
        json!({
            "namespace": NAMESPACE,
            "serviceAccount": "podid-verdict-control",
            "roleArn": role_arn(ADMITTING_ROLE),
        }),
    )
    .await;
    assert_eq!(
        refusal(&refused),
        cluster_not_found(ghost),
        "case the admitting role on a ghost cluster: expected the cluster's not-found, got \
         {refused:?}"
    );

    let listed = list_associations(&router, CLUSTER).await;
    assert_eq!(
        listed.status, 200,
        "case after the refused rows: the real cluster must still list, got {listed:?}"
    );
    assert_eq!(
        listed.body["associations"].as_array().map(Vec::len),
        Some(0),
        "case after the refused rows: a refused mutation must write nothing, got {:?}",
        listed.body["associations"]
    );

    let created = create_association(
        &router,
        REGION,
        CLUSTER,
        json!({
            "namespace": NAMESPACE,
            "serviceAccount": "podid-verdict-control",
            "roleArn": role_arn(ADMITTING_ROLE),
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case the admitting role on the cluster that resolves: the same role must be \
         accepted, got {created:?}"
    );
    let association_id = created.association()["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !association_id.is_empty(),
        "case the admitting role on the cluster that resolves: the create must report an \
         associationId, got {created:?}"
    );

    let read = call(
        &router,
        REGION,
        "GET",
        &format!(
            "/clusters/{}/pod-identity-associations/{}",
            encode_label(CLUSTER),
            encode_label(&association_id)
        ),
        "",
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case the stored association: it must read back, got {read:?}"
    );
    assert_eq!(
        read.association()["roleArn"].as_str(),
        Some(role_arn(ADMITTING_ROLE).as_str()),
        "case the stored association: it must carry the role it was created with, got {:?}",
        read.association()
    );
}
