// SPDX-License-Identifier: CC0-1.0

//! WHAT a pod-bound projected token is exchanged for, once the cluster's Kubernetes
//! API server has minted one.
//!
//! The RULE under test: a projected service account token is a REFERENCE to the
//! `(namespace, service account)` identity it names, not a grant — so every exchange
//! of it resolves the pod identity association the cluster holds for that identity AT
//! THE MOMENT OF THE CALL and mints a FRESH session of the role that association then
//! binds: the association's `roleArn`, or — when it names one — the `targetRoleArn`
//! it chains into, which is vended only if that target role's trust policy admits the
//! association's role with the `externalId` the control plane generated for the chain
//! (`PodIdentityAssociation$targetRoleArn`: "you can set a target IAM role in the
//! same or a different account"; `PodIdentityAssociation$externalId`: "the unique
//! identifier for this EKS Pod Identity association for a target IAM role"). A
//! control-plane change to the association therefore changes what the SAME token
//! yields, with no new token minted, and no exchange hands back a credential a
//! previous one vended.
//!
//! That is the far end of the data plane the Kubernetes API suite drives
//! (`tests/all/kubernetes_api.rs`): the token here is a real one, minted by the
//! cluster's own API server over the address the cluster published and under the
//! certificate authority it published beside it, so what the exchange resolves is
//! what a pod running in the cluster would present.
//!
//! The tables span what the rule generalises over: how the association came to bind
//! its current role (created with it, or re-pointed to it by an update), which
//! namespace and service account the identity is, how many times the same token is
//! exchanged, and — for the chain — whether the target role's trust policy admits the
//! association's role unconditionally, admits it on the generated external id, admits
//! it on ANOTHER external id, admits only another principal, or is a role IAM holds no
//! record of at all.
//!
//! IAM and STS are mounted beside the two EKS planes here, over ONE shared engine: a
//! role chain is decided by a trust policy IAM holds, and the credential vended is a
//! session recorded where every service resolves a temporary credential's principal.

use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_eks_awswesleyfrontend::state::POD_IDENTITY_SERVICE_PRINCIPAL;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_state;
use smithy_server_runtime::MultiServiceRouter;

// Every harness this suite needs already exists in the crate's test tree, so it is
// reused rather than copied: the wire plumbing (`wire.rs`), the cluster drivers
// (`certificate_authority.rs`), the Kubernetes client that dials the published
// endpoint under the published certificate authority (`kubernetes_api.rs`), the
// association drivers (`pod_identity_associations.rs`) and the IAM role setup
// (`pod_identity_resolves_the_cluster_first.rs`).
use crate::certificate_authority::{str_member, REGION};
use crate::kubernetes_api::{
    assume_role_for_pod_identity, dial, emulator_with_pod_identity, provisioned_cluster,
    Kubeconfig, NO_ASSOCIATION_REFUSAL, PODS_AUDIENCE, TARGET_ROLE_REFUSAL,
};
use crate::pod_identity_associations::{association_id_of, create_association, update_association};
use crate::pod_identity_resolves_the_cluster_first::{
    create_role, role_arn, service_trust_policy,
};
use crate::wire::{error_code, Answer};

/// The namespace Kubernetes creates with a cluster, which most rows run in.
const DEFAULT_NAMESPACE: &str = "default";

/// A trust policy that lets the EKS Pod Identity service principal assume the role —
/// what an association's own role must carry for the control plane to point an
/// association at it.
fn pod_identity_trust_policy() -> String {
    service_trust_policy(POD_IDENTITY_SERVICE_PRINCIPAL)
}

/// A trust policy that lets `principal` assume the role, narrowed to an
/// `sts:ExternalId` when `external_id` is given — the form AWS publishes for a role
/// another role assumes with an external id.
fn chained_trust_policy(principal: &str, external_id: Option<&str>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "AWS": principal },
        "Action": ["sts:AssumeRole"],
    });
    if let Some(external_id) = external_id {
        statement["Condition"] = json!({ "StringEquals": { "sts:ExternalId": external_id } });
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// The emulator the rows drive: the EKS control plane, the EKS Auth plane, IAM and STS
/// over ONE shared engine, with the engine handle so a suite can read the SESSION an
/// exchange vended where every service resolves a temporary credential's principal.
///
/// The two EKS planes come from the Kubernetes API suite's own builder
/// ([`emulator_with_pod_identity`]) rather than being mounted again; what this one adds
/// is IAM and STS, because a role chain is decided by a trust policy IAM holds.
fn emulator_with_pod_identity_and_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (mut router, engine) = emulator_with_pod_identity();
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// The identity a row's token names, and the Pod the token is bound to.
#[derive(Debug, Clone, Copy)]
struct PodIdentity {
    namespace: &'static str,
    /// Whether the namespace has to be created first, or is one Kubernetes creates
    /// with the cluster.
    create_namespace: bool,
    service_account: &'static str,
    pod: &'static str,
}

/// Mint a pod-bound projected token for `identity` over the cluster's OWN Kubernetes
/// API: the namespace and service account it names are created where the row asks for
/// them, then the Pod object, then the `TokenRequest` bound to that Pod.
///
/// What comes back is the only token class `AssumeRoleForPodIdentity` accepts, so an
/// exchange of it is the exchange a pod makes.
async fn pod_bound_token(config: &Kubeconfig, case: &str, identity: &PodIdentity) -> String {
    if identity.create_namespace {
        let created = dial(
            config,
            case,
            "POST",
            "/api/v1/namespaces",
            Some(&json!({ "metadata": { "name": identity.namespace } })),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected the namespace to be created, got {created:?}"
        );
    }
    let created = dial(
        config,
        case,
        "POST",
        &format!("/api/v1/namespaces/{}/serviceaccounts", identity.namespace),
        Some(&json!({ "metadata": { "name": identity.service_account } })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected the service account to be created, got {created:?}"
    );
    let pod = dial(
        config,
        case,
        "POST",
        &format!("/api/v1/namespaces/{}/pods", identity.namespace),
        Some(&json!({
            "metadata": { "name": identity.pod },
            "spec": {
                "serviceAccountName": identity.service_account,
                "containers": [{ "name": "app", "image": "public.ecr.aws/app:1" }],
            },
        })),
    )
    .await;
    assert_eq!(
        pod.status, 201,
        "case {case:?}: expected the pod to be created, got {pod:?}"
    );
    let uid = str_member(case, "metadata.uid", &pod.body["metadata"]["uid"]);
    let minted = dial(
        config,
        case,
        "POST",
        &format!(
            "/api/v1/namespaces/{}/serviceaccounts/{}/token",
            identity.namespace, identity.service_account
        ),
        Some(&json!({
            "kind": "TokenRequest",
            "spec": {
                "audiences": [PODS_AUDIENCE],
                "boundObjectRef": { "kind": "Pod", "name": identity.pod, "uid": uid },
            },
        })),
    )
    .await;
    assert_eq!(
        minted.status, 201,
        "case {case:?}: expected the token to be issued, got {minted:?}"
    );
    let token = str_member(case, "status.token", &minted.body["status"]["token"]);
    assert!(
        !token.is_empty(),
        "case {case:?}: the answer must carry the minted token, got {:?}",
        minted.body
    );
    token
}

/// The SESSION an exchange vended, read where every service resolves a temporary
/// credential's principal — so what is asserted is the credential's own record, not
/// the wording of the answer that carried it.
fn vended_session(
    engine: &engine::InMemoryEngine,
    case: &str,
    answer: &Answer,
) -> sts_state::Session {
    let access_key_id = str_member(
        case,
        "credentials.accessKeyId",
        &answer.body["credentials"]["accessKeyId"],
    );
    sts_state::service_state(engine)
        .expect("the shared engine carries the token service's session registry")
        .lock()
        .session(&access_key_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the vended credential must name a recorded session, got \
                 {answer:?}"
            )
        })
}

/// Assert that `answer` vended a fresh session of `expected_role_arn`, and answer the
/// access key id it was vended under so a caller can show the next one differs.
fn assert_session_of(
    engine: &engine::InMemoryEngine,
    case: &str,
    answer: &Answer,
    expected_role_arn: &str,
) -> String {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the pod-bound token must exchange for credentials, got {answer:?}"
    );
    let session = vended_session(engine, case, answer);
    assert_eq!(
        session.role_arn.as_deref(),
        Some(expected_role_arn),
        "case {case:?}: the session must be one of {expected_role_arn}, got {session:?}"
    );
    let role_name = expected_role_arn
        .rsplit('/')
        .next()
        .expect("a role ARN names the role");
    let assumed = str_member(
        case,
        "assumedRoleUser.arn",
        &answer.body["assumedRoleUser"]["arn"],
    );
    assert!(
        assumed.contains(&format!("assumed-role/{role_name}/")),
        "case {case:?}: the answer must name a session of {role_name}, got {assumed:?}"
    );
    session.access_key_id
}

// ---------------------------------------------------------------------------
// RULE: the exchange resolves the association's CURRENT role, so re-pointing the
// association changes what the same token vends — and every exchange mints a fresh
// credential rather than handing back one already vended.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_token_vends_the_role_the_association_currently_binds() {
    let (router, engine) = emulator_with_pod_identity_and_iam();
    let cluster = "podid-exchange-repoint";
    let config = provisioned_cluster(&router, REGION, cluster).await;

    // Three roles IAM holds, each admitting the EKS Pod Identity principal, so an
    // association may be pointed at any of them.
    for role in ["podIdExchangeOne", "podIdExchangeTwo", "podIdExchangeThree"] {
        create_role(&router, role, pod_identity_trust_policy()).await;
    }

    // The rows differ along what the rule generalises over: the identity the token
    // names (the namespace Kubernetes creates with the cluster and one the client
    // created), and which role the association binds before and after the update.
    let rows = [
        (
            "a service account in the default namespace, re-pointed to another role",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "checkout",
                pod: "pod-checkout",
            },
            "podIdExchangeOne",
            "podIdExchangeTwo",
        ),
        (
            "a service account in a namespace the client created, re-pointed back",
            PodIdentity {
                namespace: "team-ledger",
                create_namespace: true,
                service_account: "ledger",
                pod: "pod-ledger",
            },
            "podIdExchangeTwo",
            "podIdExchangeOne",
        ),
        (
            "a third identity, re-pointed to a role no other row binds",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "reporting",
                pod: "pod-reporting",
            },
            "podIdExchangeOne",
            "podIdExchangeThree",
        ),
    ];

    for (case, identity, first_role, second_role) in rows {
        let token = pod_bound_token(&config, case, &identity).await;

        // Before the control plane binds the identity to a role there is nothing to
        // resolve: the token names an identity the cluster holds no association for.
        let unassociated = assume_role_for_pod_identity(&router, REGION, cluster, &token).await;
        assert_eq!(
            unassociated.status, 404,
            "case {case:?}: a token with no association must resolve to nothing, got \
             {unassociated:?}"
        );
        assert_eq!(
            error_code(&unassociated),
            "ResourceNotFoundException",
            "case {case:?}: the refusal names the resource that is missing, got {unassociated:?}"
        );
        assert_eq!(
            unassociated.message(),
            NO_ASSOCIATION_REFUSAL,
            "case {case:?}: expected the no-association sentence, got {:?}",
            unassociated.message()
        );

        let created = create_association(
            &router,
            REGION,
            cluster,
            &json!({
                "namespace": identity.namespace,
                "serviceAccount": identity.service_account,
                "roleArn": role_arn(first_role),
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreatePodIdentityAssociation, got {created:?}"
        );
        let association_id = association_id_of(&created);

        // The role the association binds NOW, twice: the second exchange is a fresh
        // session rather than the first one handed out again.
        let first = assume_role_for_pod_identity(&router, REGION, cluster, &token).await;
        let first_key = assert_session_of(&engine, case, &first, &role_arn(first_role));
        let again = assume_role_for_pod_identity(&router, REGION, cluster, &token).await;
        let again_key = assert_session_of(&engine, case, &again, &role_arn(first_role));
        assert_ne!(
            first_key, again_key,
            "case {case:?}: each exchange mints its own credential, got {first_key} twice"
        );

        // The control plane re-points the association. No new token is minted: what
        // changes is the state the SAME token resolves against.
        let updated = update_association(
            &router,
            REGION,
            cluster,
            &association_id,
            &json!({ "roleArn": role_arn(second_role) }),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200 from UpdatePodIdentityAssociation, got {updated:?}"
        );
        assert_eq!(
            updated.association()["roleArn"],
            json!(role_arn(second_role)),
            "case {case:?}: the update must report the role it bound, got {:?}",
            updated.association()
        );

        let after_update = assume_role_for_pod_identity(&router, REGION, cluster, &token).await;
        let after_key = assert_session_of(&engine, case, &after_update, &role_arn(second_role));
        assert_ne!(
            after_key, first_key,
            "case {case:?}: the exchange after the update must mint its own credential, got the \
             first one again"
        );
        assert_ne!(
            after_key, again_key,
            "case {case:?}: the exchange after the update must mint its own credential, got the \
             second one again"
        );
        assert_eq!(
            after_update.body["podIdentityAssociation"]["associationId"],
            json!(association_id),
            "case {case:?}: the exchange must name the association it resolved, got {:?}",
            after_update.body
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: an association that names a TARGET role vends a session of THAT role, and
// only where the target role's trust policy admits the association's role with the
// external id the control plane generated for the chain.
// ---------------------------------------------------------------------------

/// What a row's target role's trust policy admits.
#[derive(Debug, Clone, Copy)]
enum TargetTrust {
    /// The association's role, narrowed to the external id the control plane generated
    /// for this association's chain.
    OnTheGeneratedExternalId,
    /// The association's role, with no condition at all.
    Unconditionally,
    /// The association's role, narrowed to ANOTHER external id than the generated one.
    OnAnotherExternalId,
    /// Another principal than the association's role.
    AnotherPrincipal,
    /// Nothing: IAM holds no record of the target role.
    NotHeldByIam,
}

/// A role of this account that no association names — the principal the
/// [`TargetTrust::AnotherPrincipal`] row's trust policy admits instead.
const STRANGER_ROLE: &str = "podIdChainStranger";

#[tokio::test]
async fn a_chained_target_role_is_vended_only_where_its_trust_policy_admits_the_chain() {
    let (router, engine) = emulator_with_pod_identity_and_iam();
    let cluster = "podid-exchange-chain";
    let config = provisioned_cluster(&router, REGION, cluster).await;

    let association_role = "podIdChainAssociationRole";
    create_role(&router, association_role, pod_identity_trust_policy()).await;
    // A role IAM holds that no association names, so a trust policy naming ANOTHER
    // principal names one IAM can resolve — which is what makes that row a refusal
    // about the principal rather than about a principal IAM could not read.
    create_role(&router, STRANGER_ROLE, pod_identity_trust_policy()).await;

    // The rows differ along what the rule generalises over: what the target role's
    // trust policy admits, and whether IAM holds that role at all.
    let rows = [
        (
            "a target role admitting the association's role on the generated external id",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "chain-scoped",
                pod: "pod-chain-scoped",
            },
            "podIdChainScopedTarget",
            TargetTrust::OnTheGeneratedExternalId,
            true,
        ),
        (
            "a target role admitting the association's role with no condition",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "chain-open",
                pod: "pod-chain-open",
            },
            "podIdChainOpenTarget",
            TargetTrust::Unconditionally,
            true,
        ),
        (
            "a target role narrowed to another external id",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "chain-other-id",
                pod: "pod-chain-other-id",
            },
            "podIdChainOtherIdTarget",
            TargetTrust::OnAnotherExternalId,
            false,
        ),
        (
            "a target role admitting another principal",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "chain-other-principal",
                pod: "pod-chain-other-principal",
            },
            "podIdChainOtherPrincipalTarget",
            TargetTrust::AnotherPrincipal,
            false,
        ),
        (
            "a target role IAM holds no record of",
            PodIdentity {
                namespace: DEFAULT_NAMESPACE,
                create_namespace: false,
                service_account: "chain-absent",
                pod: "pod-chain-absent",
            },
            "podIdChainAbsentTarget",
            TargetTrust::NotHeldByIam,
            false,
        ),
    ];

    for (case, identity, target_role, trust, admitted) in rows {
        let token = pod_bound_token(&config, case, &identity).await;
        let created = create_association(
            &router,
            REGION,
            cluster,
            &json!({
                "namespace": identity.namespace,
                "serviceAccount": identity.service_account,
                "roleArn": role_arn(association_role),
                "targetRoleArn": role_arn(target_role),
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreatePodIdentityAssociation, got {created:?}"
        );
        // The external id is the control plane's to generate for the chain, and it is
        // what the target role's trust policy conditions on — so the policy below is
        // written from what the association reports, never from a value this test
        // chose.
        let external_id = str_member(
            case,
            "association.externalId",
            &created.association()["externalId"],
        );
        assert!(
            !external_id.is_empty(),
            "case {case:?}: an association naming a target role reports the external id \
             generated for the chain, got {:?}",
            created.association()
        );

        match trust {
            TargetTrust::OnTheGeneratedExternalId => {
                create_role(
                    &router,
                    target_role,
                    chained_trust_policy(&role_arn(association_role), Some(&external_id)),
                )
                .await;
            }
            TargetTrust::Unconditionally => {
                create_role(
                    &router,
                    target_role,
                    chained_trust_policy(&role_arn(association_role), None),
                )
                .await;
            }
            TargetTrust::OnAnotherExternalId => {
                create_role(
                    &router,
                    target_role,
                    chained_trust_policy(
                        &role_arn(association_role),
                        Some(&format!("{external_id}-and-another")),
                    ),
                )
                .await;
            }
            TargetTrust::AnotherPrincipal => {
                create_role(
                    &router,
                    target_role,
                    chained_trust_policy(&role_arn(STRANGER_ROLE), None),
                )
                .await;
            }
            // IAM holds no record of this role, so nothing is created for it.
            TargetTrust::NotHeldByIam => {}
        }

        let exchanged = assume_role_for_pod_identity(&router, REGION, cluster, &token).await;
        if admitted {
            assert_session_of(&engine, case, &exchanged, &role_arn(target_role));
            let session = vended_session(&engine, case, &exchanged);
            assert_eq!(
                session.external_id.as_deref(),
                Some(external_id.as_str()),
                "case {case:?}: the chained session must record the external id the chain was \
                 assumed with, got {session:?}"
            );
            assert_ne!(
                session.role_arn.as_deref(),
                Some(role_arn(association_role).as_str()),
                "case {case:?}: the pod is handed the END of the chain, not the association's \
                 own role, got {session:?}"
            );
        } else {
            assert_eq!(
                exchanged.status, 400,
                "case {case:?}: a chain the target role does not admit must be refused, got \
                 {exchanged:?}"
            );
            assert_eq!(
                error_code(&exchanged),
                "AccessDeniedException",
                "case {case:?}: the refusal is a permission failure, got {exchanged:?}"
            );
            assert_eq!(
                exchanged.message(),
                TARGET_ROLE_REFUSAL,
                "case {case:?}: expected the target-role refusal sentence, got {:?}",
                exchanged.message()
            );
            assert!(
                exchanged.body["credentials"].is_null(),
                "case {case:?}: a refused exchange vends nothing, got {:?}",
                exchanged.body
            );
        }
    }
}
