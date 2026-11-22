// SPDX-License-Identifier: CC0-1.0

//! The ONE accepted SHADOW WRITE of this service: the merge, the version the shadow name
//! consumes and the two ceilings a stored shadow is held to.
//!
//! Every write of a shadow goes through it — the client's `UpdateThingShadow` and the
//! write the SERVICE performs for a job's package version reporting
//! ([`super::package_version_reporting`]) alike — so there is ONE sequence rather than a
//! copy per caller: the slot cap, the version the slot stands at, the merge of the patch
//! into a COPY of the stored document, the published state quota, this emulator's own cap
//! on the whole stored record, and only then the version consumed and the document stored.
//! A second copy of that sequence is how the two writers come to enforce different
//! ceilings, which is exactly what this module prevents.
//!
//! What it deliberately leaves to its callers is everything that is NOT the write: which
//! shadow names a caller may address, what a request's own document must look like, the
//! conditional-update guard on `version`, and how a refusal is worded — each of those is
//! the calling operation's contract, and the service's own write answers no caller at all.
//! The ANNOUNCEMENT is a caller's too, for one reason: it publishes with no store held, so
//! the guard this module is handed has to be released first (see
//! [`super::shadow_topics::announce_update`]).

use ::serde_json::Value;

use super::{ComAmazonawsIotdataplaneIotmoonrakerserviceState, Shadow};

/// One accepted write, as its caller needs it: what is now stored, what stood there
/// before, and the two versions and the instant that name the change.
#[derive(Debug)]
pub struct AcceptedShadowWrite {
    /// The stored document as a reader answers with it — `state`, `metadata`, `version`
    /// and `timestamp`.
    pub stored_document: Value,
    /// The document that stood there before this write, or `None` when this write created
    /// the shadow.
    pub previous: Option<Shadow>,
    /// The version the shadow name stood at before this write.
    pub previous_version: i64,
    /// The document now stored.
    pub current: Shadow,
    /// The version this write consumed.
    pub version: i64,
    /// When the write was accepted, in epoch SECONDS — the unit shadow documents use.
    pub timestamp_secs: i64,
}

/// Why a write was NOT accepted. The stored shadow is untouched in every case, and each
/// caller words the refusal in its own contract's terms.
#[derive(Debug)]
pub enum ShadowWriteRefusal {
    /// The write would need a new shadow SLOT and this emulator holds the most it tracks
    /// ([`super::MAX_SHADOWS`]).
    TooManyShadows,
    /// The state the merge would leave stored is over the published state-document quota
    /// ([`super::MAX_SHADOW_DOCUMENT_BYTES`]), carrying the size it measured.
    StateTooLarge(usize),
    /// The whole stored record — the metadata mirror and the version beside the state —
    /// is over this emulator's own cap ([`super::MAX_STORED_SHADOW_BYTES`]), carrying the
    /// size it measured.
    RecordTooLarge(usize),
}

/// Merge `desired` / `reported` into the shadow at `key` and store the result, or refuse
/// the write having changed nothing.
///
/// The merge is applied to a COPY first, because both ceilings bound the ACCUMULATED
/// document rather than one patch: a write whose result would cross either is refused
/// without having touched what is stored. The version is consumed under the same borrow
/// the slot was read through, so the version the caller reports is exactly the one the
/// slot now stands at.
pub fn apply_shadow_write(
    store: &mut ComAmazonawsIotdataplaneIotmoonrakerserviceState,
    key: &(String, String),
    desired: Option<&Value>,
    reported: Option<&Value>,
    timestamp_secs: i64,
) -> Result<AcceptedShadowWrite, ShadowWriteRefusal> {
    // Both halves of the key come from a caller, so a caller must not be able to name a
    // new shadow per call forever. A write of a shadow name that already has a slot never
    // trips this.
    if !store.shadows.contains_key(key) && store.shadows.len() >= super::MAX_SHADOWS {
        return Err(ShadowWriteRefusal::TooManyShadows);
    }

    let previous_version = store.shadow_version(key);
    let previous = store
        .stored_shadow(key)
        .map(|(document, _version)| document.clone());
    let mut merged: Shadow = previous.clone().unwrap_or_default();
    merged.apply(desired, reported, timestamp_secs);
    merged.timestamp_secs = timestamp_secs;

    let state_bytes = merged.state_bytes();
    if state_bytes > super::MAX_SHADOW_DOCUMENT_BYTES {
        return Err(ShadowWriteRefusal::StateTooLarge(state_bytes));
    }
    let version = previous_version + 1;
    let stored_document = merged.document(version);
    let stored_bytes = ::serde_json::to_vec(&stored_document)
        .expect("a shadow document serializes to JSON")
        .len();
    if stored_bytes > super::MAX_STORED_SHADOW_BYTES {
        return Err(ShadowWriteRefusal::RecordTooLarge(stored_bytes));
    }

    let slot = store.shadows.entry(key.clone()).or_default();
    // The slot's counter was read through this same borrow, so the version it consumes
    // here is exactly the `version` the document reports.
    slot.consume_version();
    let current = merged.clone();
    slot.document = Some(merged);

    Ok(AcceptedShadowWrite {
        stored_document,
        previous,
        previous_version,
        current,
        version,
        timestamp_secs,
    })
}
