// SPDX-License-Identifier: CC0-1.0

//! The absent-resource sentence of the field-level encryption surface is reached through the
//! resources' LIFECYCLE, end to end over HTTP.
//!
//! The rule under test is the one `field_level_encryption_identifier_width_and_namespace.rs`
//! states over generated identifiers, on the half of it this suite reaches: an operation
//! addressed by a field-level encryption identifier answers its one modelled not-found code
//! (404) and the ABSENT resource ("The specified field level encryption config/profile does not
//! exist.") for an identifier of the record's OWN namespace that it cannot resolve.
//!
//! This suite reads that sentence back on the identifiers this emulator really ISSUED, after
//! their resources are gone: a client holding an identifier for a resource the account no
//! longer has is the case that matters most to it.
//! The rows span both resource kinds of the surface and every operation addressed by one of
//! their identifiers — both readers, the update and the delete — and the delete order is the
//! one the service requires, since a profile a stored configuration still names is refused as
//! in use rather than deleted.
//!
//! The harness is the sibling suite's ([`crate::field_level_encryption_identifier_width_and_namespace`]):
//! one `Kind`, one seed, one set of addressing calls and one refusal assertion for both suites,
//! so neither can drift from the other.

use crate::distribution_lifecycle::{call, emulator, API_VERSION};
use crate::field_level_encryption_identifier_width_and_namespace::{
    address, assert_refusal, seed, Kind,
};

/// An identifier this emulator ISSUED answers the absent-resource sentence once its resource is
/// deleted, through every operation addressed by one.
#[tokio::test]
async fn an_issued_identifier_reads_as_absent_once_its_resource_is_deleted() {
    let router = emulator();
    let seeded = seed(&router).await;

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
            assert_refusal(
                &answer,
                &format!("{operation} at the identifier of the deleted resource ({id:?})"),
                kind.code(),
                kind.absent(),
            );
        }
    }
}
