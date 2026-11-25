// SPDX-License-Identifier: CC0-1.0

//! The WIDTH an identifier of this service's mint is written at is what separates the two
//! sentences a field-level encryption not-found code carries, end to end over HTTP.
//!
//! The rule under test: an operation addressed by a field-level encryption `Id` reads the FORM
//! this service ISSUES an identifier in — the identifier alphabet (digits and uppercase letters,
//! no separator and no lower case) at the width every identifier of this mint is issued at. A
//! value written that way is carried through to the STORE whatever body or namespace letter it
//! carries, and the record the store holds none under is the record reported ABSENT; a value of
//! that alphabet at ANY OTHER width claims to be an identifier of this mint that none of them
//! could be, so it is the identifier the service reports it cannot READ ("Id format is
//! invalid"); and a value written OUTSIDE the alphabet claims to be no identifier of this mint
//! at all, so it has no form to be wrong and is resolved against the store like any other.
//! These routes model one not-found code and no argument error
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryptionConfig,GetFieldLevelEncryptionProfile,GetFieldLevelEncryptionProfileConfig")`
//! reports the kind's not-found error and `AccessDenied` and nothing else), so the message is
//! the whole of what a caller can tell them apart by.
//!
//! The rows are PAIRS that isolate the discriminator one dimension at a time, with everything
//! else held fixed:
//!
//! * the WIDTH, at the boundary: the issued width, one character short of it and one past it,
//!   under the addressed kind's own namespace letter — so only the width differs between the
//!   absent record and the unreadable identifier;
//! * the same three widths with the body LOWER-CASED, which takes every one of them outside the
//!   alphabet and so back to the absent record — the case alone deciding whether the width is
//!   read;
//! * the NAMESPACE LETTER at the issued width: the kind's own, the sibling kind's, the one every
//!   other CloudFront entity id is drawn from, a letter no kind of this service mints under and
//!   a leading digit — every one of them the absent record, since which kind an identifier of
//!   the issued width was issued for is the store's to say;
//! * and the same letters at a width no identifier is issued at — every one of them unreadable.
//!
//! The issued width and the namespace letters are read off identifiers this emulator really
//! issued, so no width and no letter here is written out by hand. The harness is the one
//! `field_level_encryption_identifier_width_and_namespace.rs` publishes ([`Kind`], [`seed`],
//! [`address`], [`assert_refusal`], [`UNREADABLE_IDENTIFIER`]), reused rather than copied, and
//! both resource kinds are driven through every operation addressed by one of their identifiers.

use crate::distribution_lifecycle::emulator;
use crate::field_level_encryption_config_lifecycle::get_config;
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, seed, Kind, Seeded, UNREADABLE_IDENTIFIER,
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
    let letter = minted.chars().next().expect("a minted id is not empty");
    let issued = minted.len();
    let sibling = match kind {
        Kind::Config => Kind::Profile,
        Kind::Profile => Kind::Config,
    };
    let sibling_letter = seeded
        .minted(sibling)
        .chars()
        .next()
        .expect("a minted id is not empty");

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
            unreadable: width != issued,
            id: written.clone(),
        });
        rows.push(Row {
            case: format!("{label} in lower case"),
            id: written.to_lowercase(),
            unreadable: false,
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
        rows.push(Row {
            case: format!("{label} at the issued width"),
            id: format!("{opening}{}", body(issued - 1)),
            unreadable: false,
        });
        rows.push(Row {
            case: format!("{label} at twice the issued width"),
            id: format!("{opening}{}", body(issued * 2 - 1)),
            unreadable: true,
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

/// RULE: the width a value of the identifier alphabet is written at decides which of the one
/// not-found code's two sentences every operation addressed by a field-level encryption
/// identifier answers, and a value outside that alphabet is resolved against the store.
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
                // routes that read a form follow the width.
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
