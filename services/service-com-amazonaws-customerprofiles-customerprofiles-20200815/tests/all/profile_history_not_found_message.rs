// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the wording of `ListProfileHistoryRecords`' profile-not-found
//! refusal.
//!
//! The rule under test: `ListProfileHistoryRecords` refuses a `ProfileId` the named domain
//! does not hold with `ResourceNotFoundException` (404) whose `Message` names BOTH
//! identifiers the request carried — the profile asked for and the domain it was looked for
//! in — in its own fixed sentence, "Profile <ProfileId> not found in domain <DomainName>".
//! That sentence belongs to this listing alone: the operations that WRITE one profile
//! (`UpdateProfile`, `DeleteProfile`, `MergeProfiles`) word the same absence with the
//! service's generic by-id sentence, which interpolates no domain
//! (`tests/all/profile_not_found_message.rs` pins that one).
//!
//! The table's rows differ in both variable parts: the domain name spans the modelled
//! `name` shape (len 1..64, pattern `^[a-zA-Z0-9_-]+$`, per
//! `smithy_query(query="operation", operation="ListProfileHistoryRecords")`) at one
//! character, at sixty-four, and mixing letters, digits, `_` and `-`; and the id spans the
//! modelled `uuid` pattern `^[a-f0-9]{32}$` at both ends and by provenance — an id nothing
//! minted, one this domain minted and deleted, one a merge absorbed, and one another domain
//! minted.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test harness,
//! in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};
// The generic by-id sentence the WRITING operations keep, from the suite that pins it, so
// the two suites cannot drift apart.
use crate::profile_not_found_message::expected_message as generic_message;

/// The sentence this listing words an absent profile with: the id asked for and the domain
/// it was looked for in.
fn expected_message(profile_id: &str, domain: &str) -> String {
    format!("Profile {profile_id} not found in domain {domain}")
}

/// `ListProfileHistoryRecords` over the wire:
/// `POST /domains/{DomainName}/profiles/history-records`.
async fn list_history(emulator: &Emulator, domain: &str, profile_id: &str) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/profiles/history-records"),
            json!({ "ProfileId": profile_id }),
        )
        .await
}

/// The ids one domain cannot serve a history for, each with the case that produced it: ids
/// at both ends of the modelled pattern, one minted here and deleted, one a merge absorbed,
/// and one minted in `elsewhere`.
async fn unheld_ids(
    emulator: &Emulator,
    domain: &str,
    elsewhere: &str,
) -> Vec<(&'static str, String)> {
    let deleted = emulator
        .create_profile(domain, json!({ "FirstName": "Grace" }))
        .await;
    let (status, body) = emulator.delete_profile(domain, &deleted).await;
    assert_eq!(
        status, 200,
        "setup in {domain:?}: expected DeleteProfile 200 for {deleted}, got {status} {body:?}"
    );

    let survivor = emulator
        .create_profile(domain, json!({ "FirstName": "Ada" }))
        .await;
    let absorbed = emulator
        .create_profile(domain, json!({ "FirstName": "Alan" }))
        .await;
    let (status, body) = emulator
        .post(
            &format!("/domains/{domain}/profiles/objects/merge"),
            json!({ "MainProfileId": survivor, "ProfileIdsToBeMerged": [absorbed.clone()] }),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup in {domain:?}: expected MergeProfiles 200 for {absorbed}, got {status} {body:?}"
    );

    let foreign = emulator
        .create_profile(elsewhere, json!({ "FirstName": "Alonzo" }))
        .await;

    vec![
        ("the lowest id the pattern allows", "0".repeat(32)),
        ("the highest id the pattern allows", "f".repeat(32)),
        (
            "an id mixing digits and hex letters",
            "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string(),
        ),
        ("an id this domain minted and then deleted", deleted),
        ("an id a merge absorbed", absorbed),
        ("an id another domain minted", foreign),
    ]
}

/// The rule over every combination of a domain name form and an unheld-id provenance.
#[tokio::test]
async fn an_unheld_profile_is_refused_with_the_domain_qualified_sentence() {
    let emulator = Emulator::new();

    // The `name` shape's own bounds: one character, the full sixty-four, and every
    // character class the pattern admits.
    let sixty_four = format!("h{}", "i".repeat(63));
    let domains: Vec<String> = vec!["d".to_string(), sixty_four, "Mixed_dom-9".to_string()];
    // A separate domain, so one row's id is a real profile id this domain does not hold.
    let elsewhere = "elsewheredom";
    emulator.create_domain(elsewhere, 90).await;
    for domain in &domains {
        emulator.create_domain(domain, 90).await;
    }

    for domain in &domains {
        for (case, profile_id) in unheld_ids(&emulator, domain, elsewhere).await {
            let (status, body) = list_history(&emulator, domain, &profile_id).await;
            assert_eq!(
                (status, error_code(&body)),
                (404, "ResourceNotFoundException"),
                "case {case:?} in domain {domain:?}: expected 404 ResourceNotFoundException, \
                 got {status} {body:?}"
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                expected_message(&profile_id, domain),
                "case {case:?} in domain {domain:?}: expected the domain-qualified \
                 profile-not-found sentence, got {message:?}"
            );
        }
    }
}

/// The listing's sentence is its own: the operations that address a profile by id to WRITE
/// it keep the generic sentence for the very same unheld id in the very same domain, so the
/// two wordings do not collapse into one.
#[tokio::test]
async fn the_writing_operations_keep_the_generic_sentence_for_the_same_id() {
    let emulator = Emulator::new();
    let domain = "contrastdom";
    emulator.create_domain(domain, 90).await;
    let anchor = emulator
        .create_profile(domain, json!({ "FirstName": "Ada" }))
        .await;

    for profile_id in ["0".repeat(32), "f".repeat(32), "c".repeat(32)] {
        let (status, body) = list_history(&emulator, domain, &profile_id).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body).to_string()),
            (
                404,
                "ResourceNotFoundException",
                expected_message(&profile_id, domain)
            ),
            "case {profile_id:?} on ListProfileHistoryRecords: expected the \
             domain-qualified sentence, got {status} {body:?}"
        );

        let (update_status, update_body) = emulator
            .send(
                "PUT",
                &format!("/domains/{domain}/profiles"),
                Some(json!({ "ProfileId": profile_id, "FirstName": "Overwritten" })),
            )
            .await;
        let (delete_status, delete_body) = emulator.delete_profile(domain, &profile_id).await;
        let (merge_status, merge_body) = emulator
            .post(
                &format!("/domains/{domain}/profiles/objects/merge"),
                json!({ "MainProfileId": anchor, "ProfileIdsToBeMerged": [profile_id.clone()] }),
            )
            .await;
        let written = [
            ("UpdateProfile", update_status, update_body),
            ("DeleteProfile", delete_status, delete_body),
            ("MergeProfiles", merge_status, merge_body),
        ];
        for (operation, status, body) in written {
            assert_eq!(
                (status, error_code(&body), error_message(&body).to_string()),
                (
                    404,
                    "ResourceNotFoundException",
                    generic_message(&profile_id)
                ),
                "case {profile_id:?} on {operation}: expected the generic \
                 profile-not-found sentence, got {status} {body:?}"
            );
            assert!(
                !error_message(&body).contains(domain),
                "case {profile_id:?} on {operation}: expected the generic sentence not to \
                 name the domain {domain:?}, got {:?}",
                error_message(&body)
            );
        }
    }
}
