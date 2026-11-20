// SPDX-License-Identifier: CC0-1.0

//! WHAT an uploaded list's `Location` reads back as.
//!
//! # The rule this module implements
//!
//! Every uploaded-list family carries a `Location` — "the URI of the file that contains
//! the IPSet" and its threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet")`) — and that URI is the caller's own: it is what a
//! create submitted, or what the latest update replaced it with. A read REPORTS the stored
//! URI, character for character, in the spelling the writer chose: an `s3://` URI reads
//! back as that `s3://` URI, a path-style object URL as that path-style URL and a
//! virtual-hosted-style one as that one. The member is a record of what was configured, so
//! nothing about the request that reads it — the endpoint it arrived at, its host, its port
//! — takes part in the answer, and no read re-addresses, re-schemes or re-spells the value
//! the account stored.
//!
//! What the value has to BE is decided once, when it is written: a create and an update
//! both hold it to the S3 object URL syntax ([`super::s3_object_of_location`]), so a
//! stored location is always one of those spellings and a read never has to judge it
//! again.
//!
//! The one projection the four uploaded-list reads share, so none of them can drift from
//! the others.

/// The `Location` a read-back of an uploaded list reports for `stored` — that very value.
#[must_use]
pub fn stored_list_location(stored: &str) -> String {
    stored.to_string()
}
