// SPDX-License-Identifier: CC0-1.0

//! The WIDTH an identifier of this service's mint is written at is where the NAMESPACE LETTER
//! decides which of the two sentences a field-level encryption not-found code carries, end to
//! end over HTTP.
//!
//! The rule under test: an operation addressed by a field-level encryption `Id` reports the
//! identifier it cannot READ ("Id format is invalid") for every value that could never be an
//! identifier CloudFront issued for its kind — a value written OUTSIDE the identifier alphabet
//! (digits and uppercase letters, no separator and no lower case), and a value written exactly
//! the way an identifier is ISSUED, that alphabet at the width every identifier of this mint is
//! issued at, under ANOTHER kind's namespace letter, which makes it that other kind's
//! identifier. Every other value is carried through to the STORE — the kind's own letter at the
//! issued width, and any letter NARROWER than it, since such a width names no kind
//! of its own — and the record the store holds none under is the record reported ABSENT. A value
//! WIDER than the issued width is no identifier of these kinds either, whatever letter it opens
//! with, so it too is the identifier reported unreadable.
//! These routes model one not-found code and no argument error
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryptionConfig,GetFieldLevelEncryptionProfile,GetFieldLevelEncryptionProfileConfig")`
//! reports the kind's not-found error and `AccessDenied` and nothing else), so the message is
//! the whole of what a caller can tell them apart by.
//!
//! The rows are PAIRS that isolate the discriminator one dimension at a time, with everything
//! else held fixed:
//!
//! * the WIDTH, at the boundary: the issued width and one character short of it, under the
//!   addressed kind's own namespace letter — both the absent record,
//!   since a width no wider than the issued one says nothing about which kind an identifier was
//!   issued for — against one character PAST it, which no identifier of these kinds is written
//!   at and which is therefore unreadable;
//! * the same three widths with the body LOWER-CASED, which takes every one of them outside the
//!   alphabet and so to the unreadable identifier — the case alone deciding whether the value
//!   is an identifier at all;
//! * the NAMESPACE LETTER at the issued width: the kind's own and a letter no kind of this
//!   service mints under — both the absent record — against the sibling kind's and the one every
//!   other CloudFront entity id is drawn from, both unreadable, since at the issued width a
//!   letter another kind issues under makes the value that kind's identifier; a leading digit
//!   names no kind either;
//! * and the same letters at TWICE the issued width — every one of them the identifier reported
//!   unreadable, since no identifier of these kinds is written that wide.
//!
//! The issued width and the namespace letters are read off identifiers this emulator really
//! issued, so no width and no letter here is written out by hand. The harness is the one
//! `field_level_encryption_identifier_width_and_namespace.rs` publishes ([`Kind`], [`seed`],
//! [`address`], [`assert_refusal`], [`UNREADABLE_IDENTIFIER`]), reused rather than copied, and
//! both resource kinds are driven through every operation addressed by one of their identifiers.

use crate::distribution_lifecycle::emulator;
use crate::field_level_encryption_config_lifecycle::get_config;
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, could_be_issued_for_the_kind, namespace_letter_of, seed, Kind,
    Seeded, UNREADABLE_IDENTIFIER,
};
use crate::field_level_encryption_profile_lifecycle::get_profile;

/// A body of `width` characters of the identifier alphabet, cycling its digits and uppercase
/// letters so no body is one this emulator ever minted.
fn body(width: usize) -> String {
    "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        .chars()
        .cycle()
        .take(width)
        .collect()
}

/// One row: the case, the identifier submitted, and whether the route must report the
/// identifier UNREADABLE rather than the record absent.
struct Row {
    case: String,
    id: String,
    unreadable: bool,
}

/// The rows for `kind`, derived from the identifier this emulator issued for it: the widths
/// either side of the issued one, each in the alphabet and lower-cased, and the namespace
/// letters at the issued width and at another.
fn rows(kind: Kind, seeded: &Seeded) -> Vec<Row> {
    let minted = seeded.minted(kind);
    let letter = namespace_letter_of(minted);
    let issued = minted.len();
    let sibling = match kind {
        Kind::Config => Kind::Profile,
        Kind::Profile => Kind::Config,
    };
    let sibling_letter = namespace_letter_of(seeded.minted(sibling));

    let mut rows: Vec<Row> = Vec::new();
    // The WIDTH boundary under the kind's own letter, and the same bodies lower-cased.
    for (label, width) in [
        ("one character short of the issued width", issued - 1),
        ("the issued width", issued),
        ("one character past the issued width", issued + 1),
    ] {
        let written = format!("{letter}{}", body(width - 1));
        rows.push(Row {
            case: format!("{label} in the identifier alphabet"),
            unreadable: !could_be_issued_for_the_kind(&written, minted),
            id: written.clone(),
        });
        rows.push(Row {
            case: format!("{label} in lower case"),
            id: written.to_lowercase(),
            unreadable: true,
        });
    }
    // The NAMESPACE LETTER, at the issued width and at one no identifier is issued at.
    for (label, opening) in [
        ("the addressed kind's own namespace letter", letter),
        ("the sibling kind's namespace letter", sibling_letter),
        ("the namespace every other CloudFront entity id is drawn from", 'E'),
        ("a namespace letter no kind of this service mints", 'W'),
        ("a leading digit", '7'),
    ] {
        let at_the_issued_width = format!("{opening}{}", body(issued - 1));
        rows.push(Row {
            case: format!("{label} at the issued width"),
            unreadable: !could_be_issued_for_the_kind(&at_the_issued_width, minted),
            id: at_the_issued_width,
        });
        let at_twice_the_issued_width = format!("{opening}{}", body(issued * 2 - 1));
        rows.push(Row {
            case: format!("{label} at twice the issued width"),
            unreadable: !could_be_issued_for_the_kind(&at_twice_the_issued_width, minted),
            id: at_twice_the_issued_width,
        });
    }
    for row in &rows {
        assert_ne!(
            row.id, minted,
            "case {:?}: the submitted identifier must not be the one the store holds",
            row.case
        );
    }
    rows
}

/// RULE: whether a value could be an identifier CloudFront issued for the addressed kind — the
/// identifier alphabet, and at the issued width the kind's own namespace letter — decides which
/// of the one not-found code's two sentences every operation addressed by a field-level
/// encryption identifier answers.
#[tokio::test]
async fn the_issued_width_decides_which_sentence_an_unresolved_identifier_earns() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        for row in rows(kind, &seeded) {
            for (operation, answer) in address(&router, kind, &row.id, &seeded).await {
                let case = format!("{operation} with {} ({:?})", row.case, row.id);
                // The configuration's RESOURCE read reads no form at all, so it reports the
                // record absent for every value — which is what its own suite states; the
                // routes that read a form follow the rule above.
                let expected = if row.unreadable && operation != "GetFieldLevelEncryption" {
                    UNREADABLE_IDENTIFIER
                } else {
                    kind.absent()
                };
                assert_refusal(&answer, &case, kind.code(), expected);
            }
        }
    }

    // Every refusal above answered an identifier rather than a store this suite broke: both
    // seeded records are still served at their own identifiers.
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
