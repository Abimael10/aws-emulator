// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two admission rules `CreateFargateProfile` applies
//! beyond its payload, driven over the wire through the same `MultiServiceRouter`
//! the production binary mounts.
//!
//! 1. A create under a name the parent resource already holds is a CONFLICT with the
//!    resource that holds it: it is refused `ResourceInUseException` (409) — the
//!    conflict this service models, the one every other create of it answers a taken
//!    name with — in the fixed sentence Amazon EKS states for a Fargate profile,
//!    never as a malformed or invalid request. A client that re-drives a create after
//!    a timeout branches on that code to adopt the resource that is already there, so
//!    a parameter error in its place reads as a permanent template failure.
//! 2. A role a request hands Amazon EKS to ASSUME is usable only if the role's trust
//!    policy admits the service principal that would assume it: a Fargate profile's
//!    `podExecutionRoleArn` is assumed by the Fargate Pod plane
//!    ([`FARGATE_PODS_SERVICE_PRINCIPAL`]), so a role whose trust policy does not
//!    admit that principal is refused 400 `InvalidParameterException` in the fixed
//!    sentence Amazon EKS states for it, and no profile is left behind. The refusal
//!    is grounded in the trust policy IAM HOLDS: a role IAM has no record of, and an
//!    emulator with no IAM to ask, refuse nothing.
//!
//! The rows differ along what each rule generalises over: for the conflict, the form
//! and length of the profile name, which cluster holds it and whether the name is
//! taken in THAT cluster at all; for the trust policy, which principals the role
//! admits, how many of them the document names, whether the statement allows or
//! denies the assume, and whether IAM holds the role at all.
//!
//! Rule 2 needs IAM beside this service on one engine, which is what
//! [`crate::wire::emulator_with_identity`] mounts; the trust policies and the
//! `CreateRole` call are the shared ones in
//! `tests/all/pod_identity_resolves_the_cluster_first.rs`.

use serde_json::json;
use service_com_amazonaws_eks_awswesleyfrontend::state::{
    fargate_pod_execution_role_trust_refusal_message, FARGATE_PODS_SERVICE_PRINCIPAL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
use crate::fargate_profiles::PROFILE_NAME_IN_USE;
use crate::pod_identity_resolves_the_cluster_first::{create_role, role_arn, service_trust_policy};
use crate::wire::{call, emulator, emulator_with_identity, encode_label, error_code, Answer};

/// The role every row that is not ABOUT the pod execution role hands the create: a
/// role of the account this emulator serves, so the pass-role authorization (403) is
/// never what answers a row here.
fn default_pod_execution_role() -> String {
    role_arn("eksFargatePodExecutionRole")
}

async fn create_cluster(router: &MultiServiceRouter, name: &str) -> Answer {
    let body = json!({
        "name": name,
        "roleArn": role_arn("eksClusterRole"),
        "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
    });
    let answer = call(router, REGION, "POST", "/clusters", &body.to_string()).await;
    assert_eq!(
        answer.status, 200,
        "setup: the cluster {name:?} must be created, got {answer:?}"
    );
    answer
}

async fn create_profile(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    pod_execution_role: &str,
) -> Answer {
    let body = json!({
        "fargateProfileName": name,
        "podExecutionRoleArn": pod_execution_role,
        "selectors": [{ "namespace": "default" }],
    });
    call(
        router,
        REGION,
        "POST",
        &format!("/clusters/{}/fargate-profiles", encode_label(cluster)),
        &body.to_string(),
    )
    .await
}

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

async fn list_profiles(router: &MultiServiceRouter, cluster: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/clusters/{}/fargate-profiles", encode_label(cluster)),
        "",
    )
    .await
}

/// Whether `ListFargateProfiles` names `name` for that cluster.
fn lists(answer: &Answer, name: &str) -> bool {
    answer.body["fargateProfileNames"]
        .as_array()
        .is_some_and(|names| names.iter().any(|listed| listed == name))
}

// ---------------------------------------------------------------------------
// RULE 1: a `CreateFargateProfile` whose `(cluster, fargateProfileName)` pair is
// already taken is refused 409 `ResourceInUseException` with this operation's own
// fixed sentence, and the profile that holds the name is untouched.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_taken_profile_name_is_refused_as_the_conflict_it_is() {
    let router = emulator();

    // Two clusters, so a row can show the same name being FREE in the other one: a
    // Fargate profile name identifies a profile within its cluster.
    let first_cluster = "fp-conflict-cluster";
    let second_cluster = "fp-conflict-cluster-two";
    create_cluster(&router, first_cluster).await;
    create_cluster(&router, second_cluster).await;

    // The rows differ along the form and the length of the name — the model states no
    // pattern for `CreateFargateProfileRequest$fargateProfileName`, so a plain, a
    // hyphenated, an underscored and a long name are all names a caller may ask for —
    // and along the cluster the pair names.
    let rows: [(&str, &str, &str); 5] = [
        ("the shortest name", first_cluster, "f"),
        ("a hyphenated name", first_cluster, "fp-blue-2026"),
        ("an underscored name", first_cluster, "fp_with_underscores"),
        (
            "a long name",
            first_cluster,
            "fp-0123456789-0123456789-0123456789-0123456789-0123456789-abc",
        ),
        (
            "the same name in the other cluster",
            second_cluster,
            "fp-blue-2026",
        ),
    ];

    let role = default_pod_execution_role();
    for (case, cluster, name) in rows {
        let created = create_profile(&router, cluster, name, &role).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the first create of {name:?} in {cluster:?} must be accepted, \
             got {created:?}"
        );
        let arn = created.body["fargateProfile"]["fargateProfileArn"].clone();

        let refused = create_profile(&router, cluster, name, &role).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected HTTP 409 for the taken name, got {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceInUseException",
            "case {case:?}: expected ResourceInUseException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            PROFILE_NAME_IN_USE,
            "case {case:?}: expected message {PROFILE_NAME_IN_USE:?}, got {:?}",
            refused.message()
        );
        // The payload names the cluster the name is taken in — the one member
        // `ResourceInUseException` models that this conflict is about.
        assert_eq!(
            refused.body["clusterName"],
            json!(cluster),
            "case {case:?}: expected clusterName {cluster:?}, got {refused:?}"
        );

        // And the profile that holds the name is exactly as it was: a refused create
        // replaces nothing.
        let read = describe_profile(&router, cluster, name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the profile holding the name must still read back, got {read:?}"
        );
        assert_eq!(
            read.body["fargateProfile"]["fargateProfileArn"], arn,
            "case {case:?}: the refused create must leave the stored profile alone, got \
             {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a `podExecutionRoleArn` whose trust policy does not admit the Fargate Pod
// plane's service principal is refused 400 `InvalidParameterException` in the fixed
// sentence Amazon EKS states for it, and nothing is written.
// ---------------------------------------------------------------------------

/// The EC2 service principal — the one a node role trusts, which is the role a
/// caller most easily reaches for by mistake here.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// A trust policy that lets every principal in `principals` assume the role, in the
/// form AWS publishes for a list of service principals.
fn service_trust_policy_for(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A trust policy that names the right principal but DENIES the assume, so the
/// document mentions the principal without admitting it.
fn denying_trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// One row: a role IAM is given (or not), and whether a create naming it is served.
struct TrustRow {
    case: &'static str,
    role: &'static str,
    /// The trust policy IAM is given for `role`, or `None` for a role IAM holds no
    /// record of at all.
    trust: Option<String>,
    /// Whether `CreateFargateProfile` must serve a request naming that role.
    served: bool,
}

#[tokio::test]
async fn a_pod_execution_role_the_fargate_plane_cannot_assume_is_refused() {
    let router = emulator_with_identity();
    let cluster = "fp-trust-cluster";
    create_cluster(&router, cluster).await;

    let rows = [
        TrustRow {
            case: "a role trusting the Fargate Pod plane",
            role: "fargatePodExecutionRole",
            trust: Some(service_trust_policy(FARGATE_PODS_SERVICE_PRINCIPAL)),
            served: true,
        },
        TrustRow {
            case: "a role trusting the Fargate Pod plane beside another principal",
            role: "fargatePodExecutionAndNodeRole",
            trust: Some(service_trust_policy_for(&[
                EC2_PRINCIPAL,
                FARGATE_PODS_SERVICE_PRINCIPAL,
            ])),
            served: true,
        },
        TrustRow {
            case: "the node role, which trusts only EC2",
            role: "fargateNodeRole",
            trust: Some(service_trust_policy(EC2_PRINCIPAL)),
            served: false,
        },
        TrustRow {
            case: "a role trusting the Amazon EKS control plane instead",
            role: "fargateControlPlaneRole",
            trust: Some(service_trust_policy("eks.amazonaws.com")),
            served: false,
        },
        TrustRow {
            case: "a role trusting the EKS Pod Identity agent instead",
            role: "fargatePodIdentityRole",
            trust: Some(service_trust_policy("pods.eks.amazonaws.com")),
            served: false,
        },
        TrustRow {
            case: "a role whose statement names the principal but denies the assume",
            role: "fargateDenyingRole",
            trust: Some(denying_trust_policy(FARGATE_PODS_SERVICE_PRINCIPAL)),
            served: false,
        },
        TrustRow {
            // The refusal is grounded in a trust policy IAM HOLDS: a role IAM has no
            // record of has no trust policy to have read, so this rule refuses
            // nothing for it.
            case: "a role IAM holds no record of",
            role: "fargateAbsentRole",
            trust: None,
            served: true,
        },
    ];

    let expected = fargate_pod_execution_role_trust_refusal_message();
    assert!(
        expected.contains(FARGATE_PODS_SERVICE_PRINCIPAL),
        "the refusal must name the principal to add, got {expected:?}"
    );

    for (index, row) in rows.iter().enumerate() {
        let TrustRow {
            case,
            role,
            trust,
            served,
        } = row;
        if let Some(trust) = trust {
            create_role(&router, role, trust.clone()).await;
        }
        let name = format!("fp-trust-{index}");
        let answer = create_profile(&router, cluster, &name, &role_arn(role)).await;

        if *served {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a role this rule does not refuse must be served, got \
                 {answer:?}"
            );
            let read = describe_profile(&router, cluster, &name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the created profile must read back, got {read:?}"
            );
            assert_eq!(
                read.body["fargateProfile"]["podExecutionRoleArn"],
                json!(role_arn(role)),
                "case {case:?}: the stored profile must carry the role it was given, got \
                 {read:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 for a role the Fargate plane cannot assume, \
             got {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            answer.message()
        );
        // NOTHING IS LEFT BEHIND: a profile Amazon EKS refused is not a profile of
        // the cluster, so neither the read nor the listing knows the name.
        let read = describe_profile(&router, cluster, &name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create must store nothing, got {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {read:?}"
        );
        let listed = list_profiles(&router, cluster).await;
        assert!(
            !lists(&listed, &name),
            "case {case:?}: the listing must not name a refused profile, got {listed:?}"
        );
    }
}

/// With NO IAM to ask, the trust rule refuses nothing: a refusal has to be grounded
/// in what IAM answered rather than assumed from silence, which is what every other
/// role reading of this service does too.
#[tokio::test]
async fn a_pod_execution_role_is_not_refused_when_there_is_no_iam_to_ask() {
    let router = emulator();
    let cluster = "fp-no-iam-cluster";
    create_cluster(&router, cluster).await;

    let answer = create_profile(
        &router,
        cluster,
        "fp-no-iam",
        &default_pod_execution_role(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "with no IAM mounted the trust rule must refuse nothing, got {answer:?}"
    );
}
