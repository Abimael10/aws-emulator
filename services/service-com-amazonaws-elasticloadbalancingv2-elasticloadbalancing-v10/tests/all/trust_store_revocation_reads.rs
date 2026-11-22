// SPDX-License-Identifier: CC0-1.0

//! What `DescribeTrustStoreRevocations` REPORTS, end to end over the wire.
//!
//! The suite beside this one (`crate::trust_store_revocations`) pins what an add persists
//! and how the plane refuses; this one pins the read's own rule, over a SET of selections
//! rather than one request:
//!
//! > a read of a trust store's revocation files reports exactly the files that trust store
//! > holds whose ids the request named — every file it holds when the request names none —
//! > each with the trust store's ARN, the file's type and the number of entries read out of
//! > it, in the order their ids were minted, paged the way every read of this service pages;
//! > and the read leaves what a later caller sees untouched.
//!
//! `RevocationIds` is "The revocation IDs of the revocation files you want to describe"
//! (`smithy_query(query="operation",operation="DescribeTrustStoreRevocations")`: an optional
//! list beside the required `TrustStoreArn`), so it NARROWS the report; a set of ids states
//! which files to describe and not how many times or in which order to report one, and
//! `NextMarker` is "If there are additional results, this is the marker for the next set of
//! results. Otherwise, this is null."
//!
//! The rows differ along everything that rule generalises over: how many ids the request
//! names (none, one, some, all), where in the minted order they sit, whether the request
//! repeats one, whether it names them oldest or newest first, how many files the trust store
//! holds at all (including none), and the page size the walk asks for.
//!
//! The harness is the trust-store plane's own (`crate::trust_store_lifecycle` for the
//! emulator, `crate::trust_store_revocations` for the requests of this plane), so nothing
//! about how a revocation file is staged or added is restated here.

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::REVOCATION_TYPE_CRL;

use crate::provisioning_loop::p;
use crate::trust_store_lifecycle::{emulator_with_bundles, Emulator, BUNDLE_BUCKET};
use crate::trust_store_revocations::{
    add_revocation, authority, crl_pem, describe_revocations, reported_revocations,
    revocation_id_fields, total_revoked_entries, trust_store, walk_revocations,
};

/// How many certificates each revocation file staged below revokes, oldest file first. The
/// counts differ so a reported entry count can only come from the file it was read out of,
/// and one file revokes nothing so the rows span that edge too.
const REVOKED_PER_FILE: &[usize] = &[1, 2, 3, 0];

/// One selection a caller asks for: the case, the `RevocationIds` the request carries, and
/// the revocation ids that must come back, in the order they must come back in.
struct Selection {
    case: &'static str,
    named: &'static [i64],
    reported: &'static [i64],
}

/// The selections, spanning count, position, repetition and request order.
const SELECTIONS: &[Selection] = &[
    Selection {
        case: "no id named: every file the trust store holds",
        named: &[],
        reported: &[1, 2, 3, 4],
    },
    Selection {
        case: "one id, in the middle of the minted order",
        named: &[3],
        reported: &[3],
    },
    Selection {
        case: "the oldest and the newest file",
        named: &[1, 4],
        reported: &[1, 4],
    },
    Selection {
        case: "two ids, named newest first",
        named: &[4, 2],
        reported: &[2, 4],
    },
    Selection {
        case: "the same id named twice",
        named: &[1, 1],
        reported: &[1],
    },
    Selection {
        case: "every id, in reverse",
        named: &[4, 3, 2, 1],
        reported: &[1, 2, 3, 4],
    },
];

/// A trust store holding one revocation file per entry of [`REVOKED_PER_FILE`], and its ARN.
/// The ids the trust store minted are `1..=REVOKED_PER_FILE.len()`, in that order.
async fn trust_store_with_revocations(emulator: &Emulator, name: &str) -> String {
    let (certificate, key) = authority();
    let arn = trust_store(emulator, name, &certificate).await;
    for (index, revoked) in REVOKED_PER_FILE.iter().enumerate() {
        let object = format!("{name}/list-{index}.pem");
        emulator
            .stage(&object, &crl_pem(&certificate, &key, *revoked))
            .await;
        let added = add_revocation(emulator, &arn, BUNDLE_BUCKET, &object, &[]).await;
        assert_eq!(
            added.status, 200,
            "staging file {index} of {name}: the add must succeed, got {} body {}",
            added.status, added.body
        );
    }
    arn
}

/// What a read must report for the file `revocation_id` of `trust_store_arn`: the id, its
/// type, the entries read out of its own bytes, and the trust store it is in force in.
fn expected_entry(revocation_id: i64, trust_store_arn: &str) -> (i64, String, i64, String) {
    let revoked = REVOKED_PER_FILE[revocation_id as usize - 1] as i64;
    (
        revocation_id,
        REVOCATION_TYPE_CRL.to_string(),
        revoked,
        trust_store_arn.to_string(),
    )
}

/// The entries a read must report for the ids `reported`, in minted order.
fn expected_entries(reported: &[i64], trust_store_arn: &str) -> Vec<(i64, String, i64, String)> {
    reported
        .iter()
        .map(|revocation_id| expected_entry(*revocation_id, trust_store_arn))
        .collect()
}

#[tokio::test]
async fn a_read_reports_the_files_the_request_selected_in_minted_order() {
    let emulator = emulator_with_bundles().await;
    let arn = trust_store_with_revocations(&emulator, "ts-read-selection").await;

    for row in SELECTIONS {
        let described =
            describe_revocations(&emulator, &arn, &revocation_id_fields(row.named)).await;
        assert_eq!(
            described.status, 200,
            "case {:?}: expected 200 from DescribeTrustStoreRevocations, got {} body {}",
            row.case, described.status, described.body
        );
        let answered = reported_revocations(&described);
        assert_eq!(
            answered,
            expected_entries(row.reported, &arn),
            "case {:?}: expected the files {:?} of {arn}, got {answered:?} in {}",
            row.case,
            row.reported,
            described.body
        );
        // The whole selection fits one page when the request asks for no page size, so there
        // are no additional results to resume after.
        assert_eq!(
            described.text("NextMarker"),
            None,
            "case {:?}: a read that asks for no page reports no NextMarker, got {}",
            row.case,
            described.body
        );
    }

    // Reading reports; it changes nothing a later caller sees. After every selection above,
    // the trust store still holds every file, and still counts the same revoked entries.
    let described = describe_revocations(&emulator, &arn, &[]).await;
    let answered = reported_revocations(&described);
    let every_id: Vec<i64> = (1..=REVOKED_PER_FILE.len() as i64).collect();
    assert_eq!(
        answered,
        expected_entries(&every_id, &arn),
        "after the reads above the trust store must still hold every file, got {answered:?}"
    );
    let expected_total: i64 = REVOKED_PER_FILE.iter().map(|revoked| *revoked as i64).sum();
    let total = total_revoked_entries(&emulator, &arn).await;
    assert_eq!(
        total, expected_total,
        "after the reads above expected TotalRevokedEntries {expected_total}, got {total}"
    );
}

#[tokio::test]
async fn a_paged_read_of_a_selection_walks_the_selected_files_once() {
    let emulator = emulator_with_bundles().await;
    let arn = trust_store_with_revocations(&emulator, "ts-read-paged-selection").await;

    // A selection that leaves a file out on either side of a boundary, so a walk that paged
    // the whole collection instead of the selection could not pass. The request names those
    // very ids newest first, so the order walked cannot be the request's own.
    let selected: &[i64] = &[1, 2, 4];
    let named: Vec<i64> = selected.iter().rev().copied().collect();
    let fields = revocation_id_fields(&named);
    for page_size in 1..=selected.len() + 1 {
        let walked = walk_revocations(&emulator, &arn, &fields, page_size, selected.len()).await;
        assert_eq!(
            walked,
            selected.to_vec(),
            "case PageSize {page_size}: expected every selected file once in minted order, \
             got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_read_of_a_trust_store_holding_no_revocation_file_reports_none() {
    let emulator = emulator_with_bundles().await;
    let (certificate, _key) = authority();
    let arn = trust_store(&emulator, "ts-read-empty", &certificate).await;

    for (case, fields) in [
        ("no id named", Vec::new()),
        ("a page asked for", vec![p("PageSize", "1")]),
    ] {
        let described = describe_revocations(&emulator, &arn, &fields).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: a trust store holding no revocation file is reported, not refused, \
             got {} body {}",
            described.status, described.body
        );
        let answered = reported_revocations(&described);
        assert!(
            answered.is_empty(),
            "case {case:?}: expected no revocation file reported, got {answered:?}"
        );
        assert_eq!(
            described.text("NextMarker"),
            None,
            "case {case:?}: expected no NextMarker, got {}",
            described.body
        );
    }
}
