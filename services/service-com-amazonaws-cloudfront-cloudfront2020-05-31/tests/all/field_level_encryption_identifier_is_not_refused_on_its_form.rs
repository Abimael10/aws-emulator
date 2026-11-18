// SPDX-License-Identifier: CC0-1.0

//! A field-level encryption path identifier is carried through to the STORE unless it is
//! outside the alphabet CloudFront issues identifiers in, end to end over HTTP.
//!
//! The rule under test: an operation addressed by a field-level encryption `Id` reads only the
//! identifier ALPHABET of the value it was handed — the decimal digits and uppercase ASCII
//! letters CloudFront writes an identifier in. The member is a plain `string` in the model, with
//! no `@pattern` and no `@length`
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryption")` reports `Id` as a
//! required `httpLabel` targeting `string` and the operation's errors as `AccessDenied` and
//! `NoSuchFieldLevelEncryptionConfig` alone), so neither the namespace letter nor the body width
//! this emulator happens to MINT is a constraint a request may be refused on: every value of
//! that alphabet the account holds no record under is the record reported ABSENT ("The specified
//! field level encryption config/profile does not exist."), and only a value outside the
//! alphabet is the identifier the service reports it cannot READ ("Id format is invalid"). Both
//! sentences travel under the kind's one modelled not-found code on 404, so the MESSAGE is all
//! that separates the two conditions, and both are fixed text that echoes nothing submitted.
//!
//! The rows vary what the rule generalises over, in the one dimension it turns on — the
//! CHARACTERS of the submitted value — while the namespace letter and the width vary freely
//! underneath it:
//!
//! * identifiers of the alphabet under a namespace letter no field-level encryption record is
//!   minted under, at the width AWS writes an example identifier of this service at
//!   (`EDFDVBD6EXAMPLE`, from the identifiers AWS's own CloudFront documentation uses), at one
//!   character, at a width far past any this service issues, and one written only in digits —
//!   each the record reported absent;
//! * and the same identifier taken out of the alphabet one character at a time — lower-cased,
//!   with a hyphen, with an underscore — beside a UUID and a bare word, each the identifier
//!   reported unreadable.
//!
//! Both resource kinds of the surface are driven through every operation addressed by one of
//! their identifiers — both readers, the update and the delete — and both stored resources are
//! read back afterwards, so each sentence answered an identifier rather than a store this suite
//! broke.
//!
//! The harness is the one `field_level_encryption_identifier_width_and_namespace.rs` publishes
//! ([`Kind`], [`seed`], [`address`], [`assert_refusal`]), reused rather than copied.

use crate::distribution_lifecycle::emulator;
use crate::field_level_encryption_config_lifecycle::get_config;
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, seed, Kind,
};
use crate::field_level_encryption_profile_lifecycle::get_profile;

/// The sentence this surface answers a value outside the identifier alphabet, spelled out here
/// rather than taken from the crate, so a reworded refusal fails this suite.
const UNREADABLE_IDENTIFIER: &str = "Id format is invalid";

/// The identifier AWS's own CloudFront documentation writes in its examples: an `E` followed by
/// a body 14 characters wide — one character wider than the bodies this emulator mints, and a
/// namespace letter no field-level encryption record is minted under, so it is an identifier of
/// the alphabet that nothing in the store can answer to.
const DOCUMENTED_EXAMPLE_ID: &str = "EDFDVBD6EXAMPLE";

/// Every value of the identifier alphabet this suite submits where an identifier of a
/// field-level encryption record belongs: `(case, the Id submitted)`.
fn of_the_alphabet() -> Vec<(&'static str, String)> {
    vec![
        (
            "the identifier AWS's documentation writes in its examples",
            DOCUMENTED_EXAMPLE_ID.to_string(),
        ),
        ("a single uppercase letter", "A".to_string()),
        ("a single digit", "7".to_string()),
        (
            "a body far wider than any this service issues",
            format!("E{}", "A".repeat(63)),
        ),
        (
            "an identifier written only in digits",
            "00000000000000".to_string(),
        ),
    ]
}

/// Every value this suite submits that is OUTSIDE the identifier alphabet: the documented
/// example identifier taken out of the alphabet one character at a time, a UUID and a word.
fn outside_the_alphabet() -> Vec<(&'static str, String)> {
    vec![
        (
            "the documented example identifier in lower case",
            DOCUMENTED_EXAMPLE_ID.to_lowercase(),
        ),
        (
            "the documented example identifier with a hyphen in it",
            format!("{}-{}", &DOCUMENTED_EXAMPLE_ID[..8], &DOCUMENTED_EXAMPLE_ID[8..]),
        ),
        (
            "the documented example identifier with an underscore in it",
            format!("{}_{}", &DOCUMENTED_EXAMPLE_ID[..8], &DOCUMENTED_EXAMPLE_ID[8..]),
        ),
        (
            "a UUID, the namespace the policy kinds are keyed by",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        ("a bare word", "fle-config".to_string()),
    ]
}

/// An identifier of the alphabet CloudFront issues identifiers in is never refused on its FORM:
/// whatever letter it opens with and however wide it is, an operation of this surface that
/// cannot resolve it reports the record that is ABSENT.
#[tokio::test]
async fn an_identifier_of_the_alphabet_reports_the_record_as_absent() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        for (case, id) in of_the_alphabet() {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    kind.absent(),
                );
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

/// A value OUTSIDE that alphabet was never an identifier this service issued, so it is refused
/// as the identifier the service cannot read — the other sentence of the same code and status.
#[tokio::test]
async fn a_value_outside_the_alphabet_is_the_identifier_reported_unreadable() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        for (case, id) in outside_the_alphabet() {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    UNREADABLE_IDENTIFIER,
                );
            }
        }
    }

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
