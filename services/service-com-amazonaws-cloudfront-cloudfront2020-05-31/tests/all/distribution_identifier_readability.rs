// SPDX-License-Identifier: CC0-1.0

//! The identifier-form rows a really-minted distribution identifier is varied into, and the
//! live resources they are derived from — the fixture half of
//! [`crate::absent_identifier_is_not_a_malformed_identifier`], which drives them through the
//! routes of two different resource kinds.
//!
//! Kept beside the suite rather than inside it because the rows are about the SHAPE of a
//! CloudFront identifier — the width this emulator mints at and the ways a value can be written
//! in the identifier alphabet without ever having been issued — which is a property of the
//! service's identifiers rather than of any one route, and is therefore what a later suite
//! addressed by another kind of the same alphabet would vary over too.
//!
//! The wire harness and the create helpers are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{config, create_distribution, Answer};
use crate::field_level_encryption_identifier_width_and_namespace::in_the_identifier_alphabet;
use crate::origin_access_control_lifecycle::{create_control, table};

/// The caller reference both creates below are made under, which no row submits where an
/// identifier belongs: what these rows vary is the identifier's form, not the request document.
pub(crate) const CALLER_REFERENCE: &str = "absent-not-malformed";

/// The identifiers this account really issued for the two kinds the suite addresses.
pub(crate) struct Live {
    /// A distribution's identifier — also the minted value every row below is derived from.
    pub(crate) distribution_id: String,
    /// An origin access control's identifier: the same alphabet, issued for another kind.
    pub(crate) control_id: String,
}

/// Create one distribution and one origin access control through their own APIs, and report the
/// identifier each create handed back. Both are asserted to be written in the identifier
/// alphabet, so the rows derived from them really are of the form they claim.
pub(crate) async fn live(router: &MultiServiceRouter) -> Live {
    let created = create_distribution(
        router,
        &config("the distribution this table addresses", CALLER_REFERENCE).xml(),
    )
    .await;
    assert_created(&created, "the distribution");

    let control = table()[0].with_name("the control this table addresses", CALLER_REFERENCE);
    let control_created = create_control(router, &control.xml()).await;
    assert_created(&control_created, "the origin access control");

    let live = Live {
        distribution_id: created.root().text_at("Id").unwrap_or_default().to_string(),
        control_id: control_created
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string(),
    };
    for (case, id) in [
        ("the distribution", &live.distribution_id),
        ("the origin access control", &live.control_id),
    ] {
        assert!(
            in_the_identifier_alphabet(id),
            "case {case}: expected a minted identifier written in the identifier alphabet, got \
             {id:?}"
        );
    }
    live
}

/// Assert one create was served, naming the resource it was for.
fn assert_created(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 201,
        "case creating {case}: expected HTTP 201, got {} with {}",
        answer.status, answer.raw
    );
}

/// Values written in the identifier alphabet that no resource of either kind answers to,
/// derived from `minted` — the width this emulator really mints at — paired with the case that
/// says how each came to name nothing. `other_kind` is the really-minted identifier of a kind
/// the addressed route does NOT serve, which is the row only the STORE can tell from a value
/// the addressed kind was never issued.
pub(crate) fn alphabet_rows(minted: &str, other_kind: &str) -> Vec<(String, String)> {
    let body = &minted[1..];
    let mut short = minted.to_string();
    short.pop();

    vec![
        (
            "the minted width, never issued".to_string(),
            format!("E{}", "0".repeat(body.len())),
        ),
        ("a body one character short".to_string(), short),
        (
            "a body one character wider than this emulator mints".to_string(),
            format!("{minted}Z"),
        ),
        (
            "a far wider body than this emulator mints".to_string(),
            format!("E{body}{body}"),
        ),
        (
            "a single character of the identifier alphabet".to_string(),
            "E".to_string(),
        ),
        (
            "a namespace letter no kind of this service mints".to_string(),
            format!("W{body}"),
        ),
        (
            "a body of digits alone, under no namespace letter".to_string(),
            "0123456789012".to_string(),
        ),
        (
            "the other kind's own minted identifier".to_string(),
            other_kind.to_string(),
        ),
    ]
}
