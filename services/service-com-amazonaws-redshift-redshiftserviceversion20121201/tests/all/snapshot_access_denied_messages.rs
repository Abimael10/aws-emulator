// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE every Redshift operation refuses with `AccessToSnapshotDenied`
//! carries, end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The RULE the suite asserts: the fault's status (400) and awsQuery code
//! (`AccessToSnapshotDenied`) are what the model fixes, and the MESSAGE is ONE
//! sentence for the whole fault class — the fixed text `The account ` + ` is not
//! authorized to access the snapshot `, with exactly two variable parts, the
//! account that holds no access and then the snapshot it holds none to, and no
//! trailing punctuation. It is not the fault shape's documentation prose, and it
//! names nothing else: no wire member path, no explanation of the grant model.
//!
//! The table spans what the rule generalises over: the operation that reports
//! the fault (the revoke of a grant the snapshot does not hold, and a restore of
//! a snapshot the request says another account owns), the account (several
//! 12-digit identifiers including the all-nines bound of that width, the
//! documented Amazon Web Services Support alias, and an account whose grant an
//! earlier revoke released), the snapshot (two snapshots of the same cluster,
//! one of them stored in mixed case) and the way the request names it
//! (identifier, ARN, identifier with its cluster).

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};
use crate::snapshot_access::{
    authorize, creating_emulator, CLUSTER, OTHER_SNAPSHOT, SNAPSHOT,
};

/// The one sentence this fault class carries, spelled out here rather than read
/// off the crate under test so a row cannot pass by restating whatever the
/// handler happened to build.
fn denied(account: &str, snapshot_identifier: &str) -> String {
    format!("The account {account} is not authorized to access the snapshot {snapshot_identifier}")
}

/// Assert one refusal: HTTP 400, the awsQuery code of the fault, and the exact
/// sentence the class words.
fn assert_denied(case: &str, answer: &Answer, account: &str, snapshot_identifier: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected status 400, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code().as_deref(),
        Some("AccessToSnapshotDenied"),
        "case {case}: expected error code AccessToSnapshotDenied, got {:?} with body {}",
        answer.error_code(),
        answer.body
    );
    let expected = denied(account, snapshot_identifier);
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(expected.as_str()),
        "case {case}: expected the message {expected:?}, got {:?} with body {}",
        answer.text("Message"),
        answer.body
    );
}

/// An emulator holding the shared cluster and BOTH of its snapshots, available.
async fn two_snapshots() -> (Emulator, String) {
    let (emulator, arn) = creating_emulator(&[SNAPSHOT, OTHER_SNAPSHOT]).await;
    // Only a settled snapshot can be shared or restored, and the transition is
    // due a span after the call that minted it — crossed by advancing the
    // stopped clock, so nothing here waits on the host's speed.
    emulator.advance_clock(state::PROVISIONING_SPAN);
    (emulator, arn)
}

/// `RevokeSnapshotAccess` for `account`, naming the snapshot with `locator`.
async fn revoke(emulator: &Emulator, account: &str, locator: &[(&str, &str)]) -> Answer {
    let mut params: Vec<(&str, &str)> = vec![("AccountWithRestoreAccess", account)];
    params.extend_from_slice(locator);
    emulator.call("RevokeSnapshotAccess", &params).await
}

/// One row: the grantee the revoke names, how the request names the snapshot,
/// and which snapshot that is.
struct RevokeCase<'a> {
    label: &'a str,
    account: &'a str,
    locator: Vec<(&'a str, &'a str)>,
    /// The snapshot the refusal must name.
    snapshot: &'a str,
}

/// A revoke for an account the snapshot holds no grant for is refused with the
/// class's one sentence, naming the account the request asked for and then the
/// snapshot — over every grantee form, both snapshots and every locator.
#[tokio::test]
async fn a_revoke_without_a_grant_is_denied_with_the_class_sentence() {
    let (emulator, arn) = two_snapshots().await;
    let rows: Vec<RevokeCase> = vec![
        RevokeCase {
            label: "an account nobody granted, named by identifier",
            account: "444455556666",
            locator: vec![("SnapshotIdentifier", SNAPSHOT)],
            snapshot: SNAPSHOT,
        },
        RevokeCase {
            label: "another account on the other snapshot",
            account: "123456789012",
            locator: vec![("SnapshotIdentifier", OTHER_SNAPSHOT)],
            snapshot: OTHER_SNAPSHOT,
        },
        RevokeCase {
            label: "an account of all nines, named by ARN",
            account: "999999999999",
            locator: vec![("SnapshotArn", arn.as_str())],
            snapshot: SNAPSHOT,
        },
        RevokeCase {
            label: "the support alias, named with its cluster",
            account: state::SUPPORT_ACCOUNT_ALIAS,
            locator: vec![
                ("SnapshotIdentifier", SNAPSHOT),
                ("SnapshotClusterIdentifier", CLUSTER),
            ],
            snapshot: SNAPSHOT,
        },
    ];

    for case in &rows {
        let answer = revoke(&emulator, case.account, &case.locator).await;
        assert_denied(case.label, &answer, case.account, case.snapshot);
    }

    // And an account whose grant an earlier revoke RELEASED holds no grant any
    // more, so the same sentence refuses the second revoke of it.
    const RELEASED: &str = "777788889999";
    let granted = authorize(&emulator, RELEASED, &[("SnapshotIdentifier", SNAPSHOT)]).await;
    assert_eq!(
        granted.status, 200,
        "case seed grant {RELEASED}: expected 200, got {} with body {}",
        granted.status, granted.body
    );
    let released = revoke(&emulator, RELEASED, &[("SnapshotIdentifier", SNAPSHOT)]).await;
    assert_eq!(
        released.status, 200,
        "case the grant released: expected 200, got {} with body {}",
        released.status, released.body
    );
    let again = revoke(&emulator, RELEASED, &[("SnapshotIdentifier", SNAPSHOT)]).await;
    assert_denied(
        "the same account revoked twice",
        &again,
        RELEASED,
        SNAPSHOT,
    );
}

/// A restore of a snapshot the request says ANOTHER account owns is refused with
/// the same class sentence: the account that holds no access is this caller's
/// own, and the snapshot is the one the request named.
#[tokio::test]
async fn a_restore_of_a_foreign_owners_snapshot_is_denied_with_the_class_sentence() {
    let (emulator, _) = two_snapshots().await;
    let own_account = engine::emulator_account_id().to_string();

    // The rows differ along the owner the request claims and the snapshot it
    // names — neither of which changes the sentence's structure.
    for (case, target, snapshot, owner) in [
        (
            "a foreign owner of the mixed-case snapshot",
            "restored-denied-one",
            SNAPSHOT,
            "999988887777",
        ),
        (
            "another foreign owner of the other snapshot",
            "restored-denied-two",
            OTHER_SNAPSHOT,
            "111122223333",
        ),
    ] {
        let answer = emulator
            .call(
                "RestoreFromClusterSnapshot",
                &[
                    ("ClusterIdentifier", target),
                    ("SnapshotIdentifier", snapshot),
                    ("OwnerAccount", owner),
                ],
            )
            .await;
        assert_denied(case, &answer, &own_account, snapshot);

        // The refusal created nothing: the cluster the restore would have made
        // is not there for a later read.
        let read = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", target)])
            .await;
        assert_eq!(
            read.error_code().as_deref(),
            Some("ClusterNotFound"),
            "case {case}: expected no cluster {target} to exist, got {:?} with body {}",
            read.error_code(),
            read.body
        );
    }

    // And the same restore WITHOUT the foreign owner is served, so the rows
    // above are the owner's refusal and not an "everything is refused" artefact.
    let restored = emulator
        .call(
            "RestoreFromClusterSnapshot",
            &[
                ("ClusterIdentifier", "restored-own-account"),
                ("SnapshotIdentifier", SNAPSHOT),
                ("OwnerAccount", own_account.as_str()),
            ],
        )
        .await;
    assert_eq!(
        restored.status, 200,
        "case the account's own snapshot: expected 200, got {} with body {}",
        restored.status, restored.body
    );
}
