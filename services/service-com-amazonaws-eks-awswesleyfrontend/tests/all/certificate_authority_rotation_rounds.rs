// SPDX-License-Identifier: CC0-1.0

//! SUCCESSIVE certificate-authority rotations on one cluster, driven over the
//! wire the way a client performs them: nothing but the API and a poll loop.
//!
//! The RULE under test: every asynchronous step of a certificate-authority
//! rotation settles on its own and leaves the cluster able to take the NEXT one,
//! so a rotation is a repeatable cycle rather than a one-off. "Amazon EKS then
//! distributes the successor CA to the Amazon Web Services managed components in
//! your cluster; you can track this through the CA's distributionStatus" and the
//! "successor CA can't be activated until its distributionStatus is COMPLETE"
//! (`CreateCertificateAuthority`); the activation and the deletion are each "an
//! asynchronous operation that returns an update object", after which the
//! promoted CA signs (`IN_USE`), the outgoing one is retired (`NOT_USED`) and a
//! deleted one has left the cluster's trust bundle. So for EVERY round: append ->
//! COMPLETE -> `Successful` -> activate -> `Successful` -> `IN_USE`/`NOT_USED` ->
//! delete the CA that round replaced -> `Successful` -> gone, with the cluster
//! back at `ACTIVE` after each update and holding exactly one certificate
//! authority again, ready for the next round.
//!
//! `certificate_authority_rotation.rs` covers ONE rotation end to end. This suite
//! covers the cycle: its rows are successive ROUNDS of the same rotation on the
//! same cluster, differing in which certificate authority each round starts from
//! (the one Amazon EKS provisioned with the cluster, then one a previous round
//! promoted) and in which of the round's three calls carry an idempotency token.
//! A rotation that only worked once — because a counter, an id or a stored due
//! instant were a one-shot — fails from the second round on.
//!
//! Nothing here sleeps for a span: every wait is a poll of the read whose
//! documented condition the round is waiting for, bounded by the shared generous
//! deadline. The wire harness is the crate's one copy in `tests/all/wire.rs` and
//! every call is a real HTTP request through the same `MultiServiceRouter` the
//! production binary mounts.

use std::collections::BTreeSet;

use crate::certificate_authority::{
    append_authority, describe_authority, describe_cluster, describe_update, str_member, REGION,
};
use crate::certificate_authority_activation::activate_authority;
use crate::certificate_authority_deletion::delete_authority;
use crate::certificate_authority_listing::{list_authorities, listed_ids};
use crate::certificate_authority_rotation::{active_cluster, poll_until};
use crate::wire::{emulator, error_code};

/// One round of the rotation cycle: which of its three calls carry an
/// `@idempotencyToken`, which is what the rule generalises over besides the round
/// itself.
struct Round {
    case: &'static str,
    append_token: Option<&'static str>,
    activate_token: Option<&'static str>,
    delete_token: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// RULE: every asynchronous step of a rotation settles on its own, and a settled
// rotation leaves the cluster in the state the next rotation starts from — one
// certificate authority, signing, with no update in flight.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn successive_rotations_each_settle_and_leave_the_cluster_ready_for_the_next() {
    let router = emulator();
    let cluster = "rotation-rounds";
    active_cluster(&router, "the cluster the rounds rotate", cluster).await;

    let rounds = vec![
        Round {
            case: "round 1, from the certificate authority Amazon EKS provisioned, no tokens",
            append_token: None,
            activate_token: None,
            delete_token: None,
        },
        Round {
            case: "round 2, from the previous round's successor, every call with a token",
            append_token: Some("round-2-append"),
            activate_token: Some("round-2-activate"),
            delete_token: Some("round-2-delete"),
        },
        Round {
            case: "round 3, from the previous round's successor, only the activation with a token",
            append_token: None,
            activate_token: Some("round-3-activate"),
            delete_token: None,
        },
    ];

    // The cluster starts with the one certificate authority Amazon EKS
    // provisioned with it, and that one signs.
    let listed = list_authorities(&router, cluster, "").await;
    assert_eq!(
        listed.status, 200,
        "case {:?}: expected 200 from ListCertificateAuthorities, got {listed:?}",
        "the cluster's initial trust bundle"
    );
    let mut signer = {
        let ids = listed_ids(&listed);
        assert_eq!(
            ids.len(),
            1,
            "case {:?}: a new cluster holds exactly one certificate authority, got {listed:?}",
            "the cluster's initial trust bundle"
        );
        ids[0].clone()
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(signer.clone());

    for round in &rounds {
        let case = round.case;

        // The append answers with a successor that does not sign yet, whose trust
        // distribution is still running, plus the update that carries it.
        let appended = append_authority(&router, REGION, cluster, round.append_token).await;
        assert_eq!(
            appended.status, 200,
            "case {case:?}: expected 200 from CreateCertificateAuthority, got {appended:?}"
        );
        let successor = str_member(
            case,
            "certificateAuthority.id",
            &appended.body["certificateAuthority"]["id"],
        );
        assert_eq!(
            appended.body["certificateAuthority"]["signingStatus"],
            serde_json::json!("NOT_USED"),
            "case {case:?}: an appended successor does not sign, got {appended:?}"
        );
        assert_eq!(
            appended.body["certificateAuthority"]["distributionStatus"],
            serde_json::json!("IN_PROGRESS"),
            "case {case:?}: an appended successor starts distributing, got {appended:?}"
        );
        assert!(
            !seen.contains(&successor),
            "case {case:?}: every round's successor is a certificate authority the cluster has \
             never held; {successor:?} is one of {seen:?}"
        );
        let append_update = str_member(case, "update.id", &appended.body["update"]["id"]);

        // Distribution settles with no help at all, and so does the update that
        // carried it; the cluster then hands itself back to ACTIVE.
        let distributed = poll_until(
            case,
            "the successor's distributionStatus reaching COMPLETE",
            || describe_authority(&router, REGION, cluster, &successor),
            |answer| {
                answer.body["certificateAuthority"]["distributionStatus"]
                    == serde_json::json!("COMPLETE")
            },
        )
        .await;
        assert_eq!(
            distributed.body["certificateAuthority"]["signingStatus"],
            serde_json::json!("NOT_USED"),
            "case {case:?}: a distributed successor still does not sign, got {distributed:?}"
        );
        poll_until(
            case,
            "the append update reaching Successful",
            || describe_update(&router, REGION, cluster, &append_update),
            |answer| answer.body["update"]["status"] == serde_json::json!("Successful"),
        )
        .await;
        poll_until(
            case,
            "the cluster back at ACTIVE after the append",
            || describe_cluster(&router, REGION, cluster),
            |answer| answer.body["cluster"]["status"] == serde_json::json!("ACTIVE"),
        )
        .await;

        // The activation settles too, and the promotion it carried is observable
        // on both records and on the cluster.
        let activated =
            activate_authority(&router, REGION, cluster, &successor, round.activate_token).await;
        assert_eq!(
            activated.status, 200,
            "case {case:?}: expected 200 from ActivateCertificateAuthority, got {activated:?}"
        );
        let activation_update = str_member(case, "update.id", &activated.body["update"]["id"]);
        poll_until(
            case,
            "the activation update reaching Successful",
            || describe_update(&router, REGION, cluster, &activation_update),
            |answer| answer.body["update"]["status"] == serde_json::json!("Successful"),
        )
        .await;
        let promoted = poll_until(
            case,
            "the successor reporting IN_USE",
            || describe_authority(&router, REGION, cluster, &successor),
            |answer| {
                answer.body["certificateAuthority"]["signingStatus"] == serde_json::json!("IN_USE")
            },
        )
        .await;
        assert_eq!(
            promoted.body["certificateAuthority"]["distributionStatus"],
            serde_json::json!("COMPLETE"),
            "case {case:?}: the promoted successor stays fully distributed, got {promoted:?}"
        );
        let retired = describe_authority(&router, REGION, cluster, &signer).await;
        assert_eq!(
            retired.body["certificateAuthority"]["signingStatus"],
            serde_json::json!("NOT_USED"),
            "case {case:?}: this round's outgoing certificate authority is retired, got \
             {retired:?}"
        );
        poll_until(
            case,
            "the cluster back at ACTIVE after the activation",
            || describe_cluster(&router, REGION, cluster),
            |answer| answer.body["cluster"]["status"] == serde_json::json!("ACTIVE"),
        )
        .await;
        let described = describe_cluster(&router, REGION, cluster).await;
        assert_eq!(
            str_member(
                case,
                "cluster.certificateAuthority.active.id",
                &described.body["cluster"]["certificateAuthority"]["active"]["id"],
            ),
            successor,
            "case {case:?}: the cluster reports this round's successor as its signer, got \
             {described:?}"
        );

        // The CA this round replaced is retired, so it deletes: its removal is
        // asynchronous too and leaves the trust bundle holding the new signer
        // alone.
        let deleted = delete_authority(&router, REGION, cluster, &signer, round.delete_token).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected 200 from DeleteCertificateAuthority, got {deleted:?}"
        );
        let delete_update = str_member(case, "update.id", &deleted.body["update"]["id"]);
        poll_until(
            case,
            "the removal update reaching Successful",
            || describe_update(&router, REGION, cluster, &delete_update),
            |answer| answer.body["update"]["status"] == serde_json::json!("Successful"),
        )
        .await;
        let settled = poll_until(
            case,
            "the retired certificate authority leaving the cluster's trust bundle",
            || list_authorities(&router, cluster, ""),
            |answer| listed_ids(answer) == vec![successor.clone()],
        )
        .await;
        assert_eq!(
            settled.body["certificateAuthorities"][0]["signingStatus"],
            serde_json::json!("IN_USE"),
            "case {case:?}: the certificate authority the cluster is left with signs, got \
             {settled:?}"
        );

        // An id a round deleted resolves to nothing, whichever operation names it,
        // and the activation's refusal names the id and the cluster it was asked
        // for.
        let read = describe_authority(&router, REGION, cluster, &signer).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: reading a deleted certificate authority is 404, got {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException from the read, got {read:?}"
        );
        let reactivated = activate_authority(&router, REGION, cluster, &signer, None).await;
        assert_eq!(
            reactivated.status, 404,
            "case {case:?}: activating a deleted certificate authority is 404, got {reactivated:?}"
        );
        assert_eq!(
            error_code(&reactivated),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException from the activation, got \
             {reactivated:?}"
        );
        assert_eq!(
            reactivated.body["message"],
            serde_json::json!(format!(
                "Certificate authority {signer} is not in the trusted list for cluster {cluster}"
            )),
            "case {case:?}: expected the sentence naming the id and the cluster, got \
             {reactivated:?}"
        );

        // And the signer this round promoted cannot be deleted, which is what makes
        // the next round's append -> activate the only way out of it.
        let undeletable = delete_authority(&router, REGION, cluster, &successor, None).await;
        assert_eq!(
            undeletable.status, 409,
            "case {case:?}: deleting the active certificate authority is refused 409, got \
             {undeletable:?}"
        );
        assert_eq!(
            error_code(&undeletable),
            "ResourceInUseException",
            "case {case:?}: expected ResourceInUseException, got {undeletable:?}"
        );
        assert_eq!(
            undeletable.body["message"],
            serde_json::json!(
                "Cannot delete the active certificate authority. Activate a different CA first."
            ),
            "case {case:?}: expected AWS's sentence, got {undeletable:?}"
        );

        seen.insert(successor.clone());
        signer = successor;
    }

    // Every round ran, so the cycle is repeatable: as many successors as rounds
    // were promoted, each a distinct certificate authority, and the cluster is
    // left holding only the last one.
    assert_eq!(
        seen.len(),
        rounds.len() + 1,
        "case {:?}: one certificate authority per round plus the cluster's own, got {seen:?}",
        "the whole cycle"
    );
    let final_listing = list_authorities(&router, cluster, "").await;
    assert_eq!(
        listed_ids(&final_listing),
        vec![signer.clone()],
        "case {:?}: the cluster is left holding the last promoted certificate authority, got \
         {final_listing:?}",
        "the whole cycle"
    );
}
