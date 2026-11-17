// SPDX-License-Identifier: CC0-1.0

//! `DescribeCertificateAuthority` refuses a certificate authority the named cluster
//! does not hold the way Amazon EKS does: with the refusal the certificate-authority
//! plane answered, carried through WRAPPED.
//!
//! The RULE under test: a read Amazon EKS serves out of an internal plane reports
//! that plane's own refusal with the tail that names the call behind it — the
//! service that answered, the status it answered, the id of that call and how many
//! attempts were made — so the message is
//! `certificate authority not found (Service: EksCertificateAuthority, Status Code:
//! 404, Request ID: <request id>) (SDK Attempt Count: 1)`. Everything but the
//! request id is fixed text: the certificate authority and the cluster are the
//! request's own `@httpLabel`s, so neither is echoed. The request id identifies the
//! call that was made, so it is drawn afresh per call and two refusals never carry
//! the same one.
//!
//! The rows differ along what the rule generalises over: a certificate authority id
//! that was never minted (in and out of the UUID shape this service mints), one that
//! belongs to another cluster, one whose cluster is in another region, and — the case
//! a rotation client actually hits — the id of a certificate authority this cluster
//! HELD and a delete has taken out of its trust bundle.
//!
//! The three suites after it hold the rule fixed across the LIFECYCLE by which the
//! cluster stopped holding the certificate authority: the refusal is a fact about
//! the id no longer resolving, so a successor deleted before it ever signed, a
//! signer an activation retired and then deleted, and a successor deleted on a
//! cluster whose sole signer's delete was refused first are each refused with the
//! same fixed text — one suite per lifecycle, so no one test drives three of them.
//!
//! `ActivateCertificateAuthority` and `DeleteCertificateAuthority` word the same
//! absence as their own recordings show, so they are not asserted here: this is the
//! READ's refusal.
//!
//! The wire harness and the certificate-authority drivers are the shared ones
//! (`tests/all/wire.rs`, `tests/all/certificate_authority.rs`).

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    CA_DISTRIBUTION_DELAY, CLUSTER_ACTIVATION_DELAY, UPDATE_COMPLETION_DELAY,
};

use crate::certificate_authority::{
    append_authority, create_cluster, describe_authority, describe_update, is_uuid, settle,
    str_member, REGION,
};
use crate::certificate_authority_activation::activate_authority;
use crate::certificate_authority_deletion::delete_authority;
use crate::certificate_authority_listing::{list_authorities, listed_ids, signer_id};
use crate::certificate_authority_rotation::{active_cluster, poll_until};
use crate::wire::{emulator, error_code};

/// The second region a row is signed for.
const OTHER_REGION: &str = "eu-central-1";

/// The head of the wrapped refusal — the plane's own sentence and the tail's
/// opening up to the request id — as Amazon EKS words it. Written out here rather
/// than composed from the service's own constants, so a change to that wording
/// cannot keep this suite green.
const EXPECTED_HEAD: &str = "certificate authority not found (Service: EksCertificateAuthority, \
                             Status Code: 404, Request ID: ";

/// The tail that closes it: how many attempts were made to reach that plane.
const EXPECTED_TAIL: &str = ") (SDK Attempt Count: 1)";

/// The request id the wrapped refusal carries, from a message that matches the
/// fixed text around it.
fn request_id(case: &str, message: &str) -> String {
    assert!(
        message.starts_with(EXPECTED_HEAD),
        "case {case:?}: the refusal must begin {EXPECTED_HEAD:?}, got {message:?}"
    );
    assert!(
        message.ends_with(EXPECTED_TAIL),
        "case {case:?}: the refusal must end {EXPECTED_TAIL:?}, got {message:?}"
    );
    let id = &message[EXPECTED_HEAD.len()..message.len() - EXPECTED_TAIL.len()];
    assert!(
        is_uuid(id),
        "case {case:?}: the tail must carry the id of the call that was made, got {id:?}"
    );
    id.to_string()
}

// ---------------------------------------------------------------------------
// RULE: every certificate authority this read cannot resolve is refused 404
// `ResourceNotFoundException` with the wrapped refusal, naming nothing from the
// request and a fresh request id per call.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unresolvable_certificate_authority_read_carries_the_wrapped_refusal() {
    let router = emulator();

    // Two clusters of the same name in two regions, and a second cluster in the
    // first region, so an id can be addressed on a cluster that does not hold it.
    for region in [REGION, OTHER_REGION] {
        let created = create_cluster(&router, region, "ca-gone").await;
        assert_eq!(
            created.status, 200,
            "setup: expected 200 from CreateCluster in {region:?}, got {created:?}"
        );
    }
    let neighbour = create_cluster(&router, REGION, "ca-neighbour").await;
    assert_eq!(
        neighbour.status, 200,
        "setup: expected 200 from CreateCluster for the neighbour, got {neighbour:?}"
    );
    settle(CLUSTER_ACTIVATION_DELAY).await;

    // A successor on the neighbour: an id that exists, but not on the cluster the
    // rows address.
    let elsewhere = append_authority(&router, REGION, "ca-neighbour", None).await;
    assert_eq!(
        elsewhere.status, 200,
        "setup: expected 200 from CreateCertificateAuthority, got {elsewhere:?}"
    );
    let elsewhere_id = str_member(
        "setup",
        "certificateAuthority.id",
        &elsewhere.body["certificateAuthority"]["id"],
    );

    // A successor the cluster HELD and then had deleted: the id a rotation client
    // reads back after its teardown.
    let appended = append_authority(&router, REGION, "ca-gone", None).await;
    assert_eq!(
        appended.status, 200,
        "setup: expected 200 from CreateCertificateAuthority, got {appended:?}"
    );
    let deleted_id = str_member(
        "setup",
        "certificateAuthority.id",
        &appended.body["certificateAuthority"]["id"],
    );
    settle(CA_DISTRIBUTION_DELAY).await;
    let removed = delete_authority(&router, REGION, "ca-gone", &deleted_id, None).await;
    assert_eq!(
        removed.status, 200,
        "setup: expected 200 from DeleteCertificateAuthority, got {removed:?}"
    );
    settle(UPDATE_COMPLETION_DELAY).await;

    let cases: Vec<(&str, &str, &str, String)> = vec![
        (
            "an id in the UUID shape that was never minted",
            REGION,
            "ca-gone",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "an id that is not a UUID at all",
            REGION,
            "ca-gone",
            "not-an-id".to_string(),
        ),
        (
            "a certificate authority of another cluster",
            REGION,
            "ca-gone",
            elsewhere_id.clone(),
        ),
        (
            "a certificate authority of the same-named cluster in another region",
            OTHER_REGION,
            "ca-gone",
            deleted_id.clone(),
        ),
        (
            "a certificate authority a delete took out of the trust bundle",
            REGION,
            "ca-gone",
            deleted_id.clone(),
        ),
    ];

    let mut request_ids: BTreeSet<String> = BTreeSet::new();
    for (case, region, cluster, authority_id) in &cases {
        let answer = describe_authority(&router, region, cluster, authority_id).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404 from DescribeCertificateAuthority, got {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {answer:?}"
        );
        let message = answer.message();
        let id = request_id(case, &message);
        assert!(
            !message.contains(authority_id.as_str()) && !message.contains(cluster),
            "case {case:?}: the refusal echoes neither label of the request, got {message:?}"
        );
        assert!(
            request_ids.insert(id.clone()),
            "case {case:?}: each call carries its own request id, but {id:?} appeared twice"
        );
        // The payload names no resource of its own: see `child_resource_not_found`.
        assert!(
            answer.body.get("clusterName").is_none(),
            "case {case:?}: the payload names no cluster, got {:?}",
            answer.body
        );
    }

    // Two reads of the SAME unresolvable id carry two request ids: the id belongs to
    // the call, not to the resource that was asked for.
    let repeat = describe_authority(&router, REGION, "ca-gone", &deleted_id).await;
    let repeated_id = request_id("a repeated read", &repeat.message());
    assert!(
        !request_ids.contains(&repeated_id),
        "a repeated read draws its own request id, got {repeated_id:?} again"
    );
}

// ---------------------------------------------------------------------------
// RULE, held fixed across the LIFECYCLE that removed the certificate authority:
// the refusal reports that the id no longer resolves on the cluster, so HOW the
// cluster stopped holding it — a successor deleted before it ever signed, a
// signer an activation retired and then deleted, or a successor deleted on a
// cluster whose sole signer's delete was refused first — changes nothing about
// the message but the request id of the call behind it.
// ---------------------------------------------------------------------------

/// How a certificate authority left the cluster's trust bundle. The rows of the
/// table below differ along this and along nothing else.
#[derive(Debug, Clone, Copy)]
enum Lifecycle {
    /// Appended, fully distributed and deleted before it ever signed.
    SuccessorNeverActivated,
    /// The certificate authority Amazon EKS provisioned with the cluster, retired
    /// by an activation and then deleted.
    RetiredByAnActivation,
    /// A successor deleted on a cluster whose sole signer's delete was refused
    /// first, so a refusal preceded the delete that removed it.
    SuccessorAfterARefusedSignerDelete,
}

/// A successor appended to `cluster` and polled until it is fully distributed —
/// `COMPLETE` is the precondition an activation states — reporting its id.
async fn distributed_successor(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    cluster: &str,
) -> String {
    let appended = append_authority(router, REGION, cluster, None).await;
    assert_eq!(
        appended.status, 200,
        "case {case:?}: expected 200 from CreateCertificateAuthority, got {appended:?}"
    );
    let id = str_member(
        case,
        "certificateAuthority.id",
        &appended.body["certificateAuthority"]["id"],
    );
    poll_until(
        case,
        "the successor's distributionStatus reaching COMPLETE",
        || describe_authority(router, REGION, cluster, &id),
        |answer| {
            answer.body["certificateAuthority"]["distributionStatus"]
                == serde_json::json!("COMPLETE")
        },
    )
    .await;
    id
}

/// Delete `authority_id` from `cluster` and poll until the cluster no longer
/// lists it: the state in which the read under test is made.
async fn delete_until_gone(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    cluster: &str,
    authority_id: &str,
) {
    let deleted = delete_authority(router, REGION, cluster, authority_id, None).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected 200 from DeleteCertificateAuthority, got {deleted:?}"
    );
    poll_until(
        case,
        "the deleted certificate authority dropping out of the listing",
        || list_authorities(router, cluster, ""),
        |answer| !listed_ids(answer).iter().any(|held| held == authority_id),
    )
    .await;
}

/// Drive `cluster` through `lifecycle` and report the id it left unresolvable.
async fn authority_removed_by(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    cluster: &str,
    lifecycle: Lifecycle,
) -> String {
    active_cluster(router, case, cluster).await;
    let signer = signer_id(router, case, cluster).await;

    let gone = match lifecycle {
        Lifecycle::SuccessorNeverActivated => distributed_successor(router, case, cluster).await,
        Lifecycle::RetiredByAnActivation => {
            let successor = distributed_successor(router, case, cluster).await;
            let activated = activate_authority(router, REGION, cluster, &successor, None).await;
            assert_eq!(
                activated.status, 200,
                "case {case:?}: expected 200 from ActivateCertificateAuthority, got {activated:?}"
            );
            let update = str_member(case, "update.id", &activated.body["update"]["id"]);
            poll_until(
                case,
                "the activation update reaching Successful",
                || describe_update(router, REGION, cluster, &update),
                |answer| answer.body["update"]["status"] == serde_json::json!("Successful"),
            )
            .await;
            poll_until(
                case,
                "the outgoing certificate authority being retired to NOT_USED",
                || describe_authority(router, REGION, cluster, &signer),
                |answer| {
                    answer.body["certificateAuthority"]["signingStatus"]
                        == serde_json::json!("NOT_USED")
                },
            )
            .await;
            signer
        }
        Lifecycle::SuccessorAfterARefusedSignerDelete => {
            // The sole signer cannot be deleted, so this call removes nothing; it
            // is the refusal the lifecycle begins with.
            let refused = delete_authority(router, REGION, cluster, &signer, None).await;
            assert_eq!(
                refused.status, 409,
                "case {case:?}: deleting the sole signer is refused 409, got {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceInUseException",
                "case {case:?}: expected ResourceInUseException, got {refused:?}"
            );
            distributed_successor(router, case, cluster).await
        }
    };

    delete_until_gone(router, case, cluster, &gone).await;
    gone
}

/// Drive `cluster` through `lifecycle`, then assert the read of the id it left
/// unresolvable is refused exactly as Amazon EKS refuses it.
///
/// This is the whole rule for one lifecycle: 404, `ResourceNotFoundException`,
/// the fixed text either side of a request id in the shape the service mints,
/// neither of the request's labels echoed, a fresh request id per call, and the
/// cluster still serving the certificate authorities it did not delete.
async fn the_read_of_the_removed_authority_is_refused(
    case: &str,
    cluster: &str,
    lifecycle: Lifecycle,
) {
    let router = emulator();
    let gone = authority_removed_by(&router, case, cluster, lifecycle).await;

    let answer = describe_authority(&router, REGION, cluster, &gone).await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected 404 from DescribeCertificateAuthority, got {answer:?}"
    );
    assert_eq!(
        error_code(&answer),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {answer:?}"
    );
    let message = answer.message();
    let id = request_id(case, &message);
    assert_eq!(
        message,
        format!("{EXPECTED_HEAD}{id}{EXPECTED_TAIL}"),
        "case {case:?}: the refusal is the fixed text around the request id of the call"
    );
    assert!(
        !message.contains(gone.as_str()) && !message.contains(cluster),
        "case {case:?}: the refusal echoes neither label of the request, got {message:?}"
    );
    // The payload names no resource of its own: see `child_resource_not_found`.
    assert!(
        answer.body.get("clusterName").is_none(),
        "case {case:?}: the payload names no cluster, got {:?}",
        answer.body
    );

    // A second read of the same removed id is refused with the same fixed text and
    // its own request id: the id belongs to the call, not to what was asked for.
    let again = describe_authority(&router, REGION, cluster, &gone).await;
    let second = request_id(case, &again.message());
    assert_ne!(
        id, second,
        "case {case:?}: each read draws its own request id"
    );

    // The cluster is still there and still serves the certificate authorities it
    // did not delete: only the removed id is unresolvable.
    let listed = list_authorities(&router, cluster, "").await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListCertificateAuthorities, got {listed:?}"
    );
    let held = listed_ids(&listed);
    assert!(
        !held.is_empty() && !held.iter().any(|id| id == &gone),
        "case {case:?}: the cluster holds its remaining certificate authorities and not the \
         removed one, got {listed:?}"
    );
}

#[tokio::test]
async fn a_successor_deleted_before_it_signed_reads_back_as_the_wrapped_refusal() {
    the_read_of_the_removed_authority_is_refused(
        "a successor deleted before it ever signed",
        "ca-life-successor",
        Lifecycle::SuccessorNeverActivated,
    )
    .await;
}

#[tokio::test]
async fn a_signer_an_activation_retired_reads_back_as_the_wrapped_refusal() {
    the_read_of_the_removed_authority_is_refused(
        "a signer an activation retired, then deleted",
        "ca-life-retired",
        Lifecycle::RetiredByAnActivation,
    )
    .await;
}

#[tokio::test]
async fn a_successor_deleted_after_a_refused_signer_delete_reads_back_as_the_wrapped_refusal() {
    the_read_of_the_removed_authority_is_refused(
        "a successor deleted after the sole signer's delete was refused",
        "ca-life-refused",
        Lifecycle::SuccessorAfterARefusedSignerDelete,
    )
    .await;
}
