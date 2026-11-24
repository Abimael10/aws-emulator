// SPDX-License-Identifier: CC0-1.0

//! The boundary of the id-format refusal on the field-level encryption surface: WHICH values an
//! operation addressed by one of these identifiers refuses on their form at all, end to end over
//! HTTP.
//!
//! The rule under test: an operation addressed by a field-level encryption record's `Id` answers
//! the kind's ONE modelled not-found code and its 404 for every identifier it cannot resolve
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryption,GetFieldLevelEncryptionConfig,GetFieldLevelEncryptionProfile,GetFieldLevelEncryptionProfileConfig")`
//! reports the kind's not-found error and `AccessDenied` and nothing else, and `Id` is a plain
//! `string` `@httpLabel` with no `@pattern` and no `@length`), so the MESSAGE is the whole of
//! what separates the two conditions, and it is decided before the store is consulted:
//!
//! * the unreadable-identifier sentence ("Id format is invalid") is reserved for a value that is
//!   WRITTEN as an identifier of this service's mint — in the identifier alphabet, digits and
//!   uppercase letters with no separator and no lower case — and yet is not written the way the
//!   addressed kind's identifiers are ISSUED: at another width, or under the namespace letter of
//!   another kind this service issues identifiers under;
//! * every other value the store holds no record under earns the absent-record sentence, both a
//!   value of the kind's issued form (which identifier of that form was ever issued is a
//!   property of the store) and a value that claims to be no identifier of this mint at all —
//!   lower-cased, hyphenated, underscored, mixed-case — in which there is no identifier whose
//!   form could be wrong;
//! * and `GetFieldLevelEncryption`, the configuration's RESOURCE read, reads NO form at all —
//!   its own recorded contract — so there every value is carried through to the store and earns
//!   the absent-record sentence, which is what the shared oracle answers for it.
//!
//! The rows are therefore PAIRS that isolate each half of that discriminator, one dimension at a
//! time, across both resource kinds and every operation addressed by one of their identifiers:
//!
//! * the same body at the same WIDTH written in the identifier alphabet and written in lower
//!   case, over widths from one short of the issued width to three times it — so the case alone
//!   decides whether the form is read, and the width decides only within the alphabet;
//! * the LEADING LETTER at the issued width: the kind's own, the sibling field-level encryption
//!   kind's, the public key's, the one every other CloudFront entity id is drawn from, a letter
//!   this service issues nothing under, and a digit;
//! * the SEPARATOR a value carries — a hyphen, an underscore, a lower-cased character — placed
//!   in a body that is otherwise of the kind's own issued form.
//!
//! Every width and namespace letter is read off an identifier this emulator really issued, so
//! nothing here is a width written out by hand, and the harness ([`Kind`], [`seed`],
//! [`address`], [`assert_refusal`], [`unresolved_answer`]) is the one
//! `field_level_encryption_identifier_width_and_namespace.rs` publishes, reused rather than
//! copied — including the oracle that spells rule (b), so the two suites cannot drift apart on
//! it.

use crate::distribution_lifecycle::emulator;
use crate::field_level_encryption_config_lifecycle::get_config;
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, in_the_identifier_alphabet, seed, unresolved_answer, Kind,
};
use crate::field_level_encryption_profile_lifecycle::{get_profile, upload_key};

/// A body of `width` characters of the identifier alphabet, cycling its digits and uppercase
/// letters so no body is one this emulator ever minted.
fn body(width: usize) -> String {
    "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        .chars()
        .cycle()
        .take(width)
        .collect()
}



/// The CASE a value is written in decides whether its form is read at all: at every width, the
/// same body written in the identifier alphabet is refused on its form unless it is of the
/// kind's issued form, while the lower-cased value of that very width is carried through to the
/// store and reported as the absent record.
#[tokio::test]
async fn the_case_a_value_is_written_in_decides_whether_its_form_is_read() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        let minted = seeded.minted(kind).to_string();
        let letter = minted.chars().next().expect("a minted id is not empty");
        let issued_body = minted.len() - 1;

        let mut rows = Vec::new();
        for width in [
            issued_body - 1,
            issued_body,
            issued_body + 1,
            issued_body + 4,
            issued_body * 3,
        ] {
            let written_as_an_identifier = format!("{letter}{}", body(width));
            rows.push((
                format!("a body of {width} characters of the identifier alphabet"),
                written_as_an_identifier.clone(),
            ));
            rows.push((
                format!("the same {width}-character body in lower case"),
                written_as_an_identifier.to_lowercase(),
            ));
        }

        for (case, id) in rows {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    unresolved_answer(operation, kind, &id, &minted),
                );
            }
        }
    }

    // Every refusal above answered an identifier rather than a store this suite broke.
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
            "case {case:?}: expected it to survive every refusal, got {} with {}",
            answer.status, answer.raw
        );
    }
}

/// Within the identifier alphabet at the issued width, the LEADING LETTER decides: the kind's
/// own namespace letter and a letter this service issues no identifier under are carried
/// through to the store, while the namespace letter of another kind it does issue identifiers
/// under is the identifier reported unreadable.
#[tokio::test]
async fn the_leading_letter_decides_within_the_identifier_alphabet() {
    let router = emulator();
    let seeded = seed(&router).await;
    // A public key this service really minted, so the public key's namespace letter below is
    // the one it issues rather than one written out here.
    let public_key = upload_key(&router, 1).await;

    for kind in [Kind::Config, Kind::Profile] {
        let minted = seeded.minted(kind).to_string();
        let sibling = match kind {
            Kind::Config => seeded.minted(Kind::Profile),
            Kind::Profile => seeded.minted(Kind::Config),
        }
        .to_string();
        let issued_body = minted.len() - 1;

        let letters = [
            (
                "the kind's own namespace letter",
                minted.chars().next().expect("a minted id is not empty"),
            ),
            (
                "the sibling field-level encryption kind's namespace letter",
                sibling.chars().next().expect("a minted id is not empty"),
            ),
            (
                "the public key's namespace letter",
                public_key.chars().next().expect("a minted id is not empty"),
            ),
            ("the letter every other entity id is drawn from", 'E'),
            ("a letter this service issues no identifier under", 'W'),
            ("a leading digit", '7'),
        ];

        for (case, leading) in letters {
            let id = format!("{leading}{}", body(issued_body));
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} at the issued width ({id:?})"),
                    kind.code(),
                    unresolved_answer(operation, kind, &id, &minted),
                );
            }
        }
    }
}

/// A SEPARATOR or a lower-cased character in a body that is otherwise of the kind's own issued
/// form takes the value out of the mint's alphabet, so there is nothing in it for the service
/// to resolve: every route that reads the alphabet reports the identifier UNREADABLE and the one
/// that reads no form at all carries it through to the store ([`unresolved_answer`]).
#[tokio::test]
async fn a_separator_takes_a_value_out_of_the_mint() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        let minted = seeded.minted(kind).to_string();
        let (letter, rest) = minted.split_at(1);
        let rows = vec![
            (
                "a hyphen inside the body".to_string(),
                format!("{letter}{}-{}", &rest[..1], &rest[1..]),
            ),
            (
                "an underscore inside the body".to_string(),
                format!("{letter}{}_{}", &rest[..1], &rest[1..]),
            ),
            (
                "a hyphenated lower-case word".to_string(),
                "absent-record-id-000".to_string(),
            ),
            (
                "the kind's own letter over a lower-cased body".to_string(),
                format!("{letter}{}", rest.to_lowercase()),
            ),
        ];

        for (case, id) in rows {
            assert!(
                !in_the_identifier_alphabet(&id),
                "case {case:?}: the row must be outside the identifier alphabet, got {id:?}"
            );
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    unresolved_answer(operation, kind, &id, &minted),
                );
            }
        }
    }

    // And both seeded records are still readable at the identifiers this service issued.
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
            "case {case:?}: expected it to survive every refusal, got {} with {}",
            answer.status, answer.raw
        );
    }
}
