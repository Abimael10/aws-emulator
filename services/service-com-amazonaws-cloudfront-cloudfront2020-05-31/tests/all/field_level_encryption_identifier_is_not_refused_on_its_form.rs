// SPDX-License-Identifier: CC0-1.0

//! An identifier this service ISSUED is never refused on its form, end to end over HTTP.
//!
//! The rule under test is the other half of the one
//! `field_level_encryption_identifier_width_and_namespace.rs` states: an operation addressed by
//! a field-level encryption `Id` reads the form CloudFront ISSUES that record's identifiers in
//! — the record's own namespace letter (the configuration's `C`, the profile's `P`) over a body
//! of the issued width, written in the digits and uppercase letters CloudFront writes an
//! identifier in — and that judgement must never stand between a caller and an identifier the
//! service really handed it. So every identifier OF that form is carried through to the store:
//! it is SERVED where a record answers to it, and reported ABSENT ("The specified field level
//! encryption config/profile does not exist.") where none does — never "Id format is invalid",
//! which tells a caller its own identifier handling is broken.
//!
//! The rows vary what the rule generalises over, in the dimension that matters here — which
//! identifier of the issued form is submitted, and whether anything answers to it:
//!
//! * an identifier this emulator really MINTED, read back through every reader of its kind
//!   (served), and read again through every operation addressed by one after its record is
//!   deleted (absent);
//! * identifiers of the same form the store never held: a body of digits and letters, a body of
//!   zeros, a body of one letter, a body of digits only, and the minted identifier with one
//!   character changed — each absent;
//! * both resource kinds of this surface, through every operation addressed by one of their
//!   identifiers — both readers, the update and the delete.
//!
//! No row anywhere in this suite may answer the unreadable-identifier sentence, which is
//! asserted explicitly: that is the over-refusal this suite exists to catch.
//!
//! The harness is the one `field_level_encryption_identifier_width_and_namespace.rs` publishes
//! ([`Kind`], [`seed`], [`address`], [`assert_refusal`], [`UNREADABLE_IDENTIFIER`]), reused
//! rather than copied.

use crate::distribution_lifecycle::{call, emulator, API_VERSION};
use crate::field_level_encryption_config_lifecycle::get_config;
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, seed, Kind, Seeded, UNREADABLE_IDENTIFIER,
};
use crate::field_level_encryption_profile_lifecycle::get_profile;

/// Identifiers of the ISSUED form of `kind` that the store holds no record for: the width and
/// the namespace letter are read off an identifier this emulator really issued, so nothing here
/// is written out by hand.
fn of_the_issued_form(kind: Kind, seeded: &Seeded) -> Vec<(String, String)> {
    let minted = seeded.minted(kind);
    let (namespace, body) = minted.split_at(1);
    let width = body.len();

    let mut changed: Vec<char> = minted.chars().collect();
    if let Some(last) = changed.last_mut() {
        *last = if *last == 'A' { 'B' } else { 'A' };
    }

    let rows = vec![
        (
            "a body of digits and letters the store never held".to_string(),
            format!(
                "{namespace}{}",
                "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                    .chars()
                    .cycle()
                    .take(width)
                    .collect::<String>()
            ),
        ),
        (
            "a body of zeros".to_string(),
            format!("{namespace}{}", "0".repeat(width)),
        ),
        (
            "a body of one letter".to_string(),
            format!("{namespace}{}", "Q".repeat(width)),
        ),
        (
            "a body of digits only".to_string(),
            format!("{namespace}{}", "7".repeat(width)),
        ),
        (
            "the minted identifier with one character changed".to_string(),
            changed.into_iter().collect(),
        ),
    ];
    for (case, id) in &rows {
        assert_ne!(
            id, minted,
            "case {case:?}: the generated identifier must not be the one the store holds"
        );
    }
    rows
}

/// An identifier of the issued form that the store holds no record for reports the record as
/// ABSENT — never the unreadable identifier — through every operation addressed by one.
#[tokio::test]
async fn an_identifier_of_the_issued_form_is_carried_through_to_the_store() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        for (case, id) in of_the_issued_form(kind, &seeded) {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                let case = format!("{operation} with {case} ({id:?})");
                let actual = answer.root().find_text("Message").unwrap_or_default();
                assert_ne!(
                    actual, UNREADABLE_IDENTIFIER,
                    "case {case:?}: an identifier of the issued form must not be refused on \
                     its form, got {actual:?}"
                );
                assert_refusal(&answer, &case, kind.code(), kind.absent());
            }
        }
    }

    // Both seeded resources are still readable at their own identifiers, so every sentence
    // above answered an identifier rather than a store this suite broke.
    let config = get_config(&router, &seeded.config_id).await;
    assert_eq!(
        config.status, 200,
        "case the seeded configuration: expected it to survive every refusal, got {} with {}",
        config.status, config.raw
    );
    let profile = get_profile(&router, &seeded.profile_id).await;
    assert_eq!(
        profile.status, 200,
        "case the seeded profile: expected it to survive every refusal, got {} with {}",
        profile.status, profile.raw
    );
}

/// The identifiers this service really MINTED are served, and once their records are deleted
/// the same identifiers report the record as ABSENT — never the unreadable identifier, which
/// would tell a caller holding an identifier the service issued that it had made it up.
#[tokio::test]
async fn a_minted_identifier_is_served_and_then_reported_absent() {
    let router = emulator();
    let seeded = seed(&router).await;

    for (case, answer) in [
        (
            "the seeded configuration",
            get_config(&router, &seeded.config_id).await,
        ),
        (
            "the seeded profile",
            get_profile(&router, &seeded.profile_id).await,
        ),
    ] {
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the minted identifier to be served, got {} with {}",
            answer.status, answer.raw
        );
    }

    // The configuration names the profile, so it goes first.
    for (case, kind) in [
        ("the seeded configuration", Kind::Config),
        ("the seeded profile", Kind::Profile),
    ] {
        let removed = call(
            &router,
            "DELETE",
            &format!(
                "/{API_VERSION}/{}/{}",
                kind.segment(),
                seeded.minted(kind)
            ),
            None,
            Some(seeded.etag(kind)),
        )
        .await;
        assert_eq!(
            removed.status, 204,
            "case deleting {case}: expected HTTP 204, got {} with {}",
            removed.status, removed.raw
        );
    }

    for kind in [Kind::Config, Kind::Profile] {
        let id = seeded.minted(kind).to_string();
        for (operation, answer) in address(&router, kind, &id, &seeded).await {
            let case = format!("{operation} at the identifier of the deleted record ({id:?})");
            let actual = answer.root().find_text("Message").unwrap_or_default();
            assert_ne!(
                actual, UNREADABLE_IDENTIFIER,
                "case {case:?}: an identifier this service issued must not be refused on its \
                 form, got {actual:?}"
            );
            assert_refusal(&answer, &case, kind.code(), kind.absent());
        }
    }
}
