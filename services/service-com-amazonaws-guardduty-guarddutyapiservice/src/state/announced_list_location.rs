// SPDX-License-Identifier: CC0-1.0

//! WHAT an uploaded list's `Location` reports on a read-back: the URI the create or
//! update supplied, exactly as it was stored.
//!
//! # The rule this module implements
//!
//! Every uploaded-list family carries a `Location` — "the URI of the file that
//! contains the IPSet" and its threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet")`) — and that URI is CALLER DATA: it addresses
//! an object in another service's storage, which this service only records. So a
//! read-back round-trips it character for character, whatever endpoint the read arrives
//! at: the value a caller submitted is the value it compares the read-back against, and
//! an update compares the value it was given against the one that is stored.
//!
//! A create or an update is held to the S3 URL syntax ([`super::is_s3_location`]) and
//! stores what the caller submitted, so the validation side of that member is untouched
//! and an update still compares against the value it was given. An address this service
//! MINTS for a caller to fetch something from it would have to lead back to this
//! emulator; a pointer the caller itself supplied for another service's object is
//! neither minted here nor served here.
//!
//! The one projection the four uploaded-list reads share, so none of them can drift
//! from the others.

/// The `Location` a read-back of an uploaded list reports for `stored` — the location
/// the create or update persisted, unchanged.
#[must_use]
pub fn announced_list_location(stored: &str) -> String {
    stored.to_string()
}
