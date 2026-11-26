// SPDX-License-Identifier: CC0-1.0

//! The identifiers this service MINTS, and the functions that write them.
//!
//! Several of this API's identifiers — an export job's `JobId`, an import job's `JobId`
//! — carry no modelled pattern and are handed back by SES as UUIDs. A UUID is not merely
//! "32 hexadecimal digits in dashed 8-4-4-4-12 groups": its layout also fixes the version
//! and the variant bits, and a client that parses or round-trips one through a UUID type
//! rejects a value whose groups are the right length but whose bits say it is no UUID. So
//! an identifier this store mints in that form is RENDERED as a UUID rather than
//! assembled out of hex digits here, by the maintained `uuid` crate through
//! [`::service_common::uuid_from_random_bytes`] — the one place in this project that
//! owns the version and variant bits and the group boundaries.
//!
//! The other identifiers SES mints take the forms SES hands them back in, and each has
//! its own renderer here: a fixed count of hexadecimal characters behind a per-resource
//! prefix ([`minted_hex`], a tenant's `TenantId`), and a pair of dot-separated
//! alphanumeric labels ([`minted_label_pair`], a multi-region endpoint's `EndpointId`).
//!
//! The bytes stay this store's own: every renderer derives them from the minting store's
//! monotonic sequence number, the instant of the mint and whatever else names the
//! resource, so an id is unique across the resources of its kind and stable for as long
//! as the resource holds it (it is minted once, at create, and read back from the record
//! afterwards).

use std::hash::{DefaultHasher, Hash, Hasher};

use super::*;

/// The text of a UUID minted for one resource of a given kind.
///
/// `domain` keeps the id spaces apart — two kinds minted from the same sequence number
/// in the same instant still differ — `sequence` is the minting store's monotonic
/// counter for that kind, `now` is the instant of the mint and `parts` is whatever else
/// names the resource (its name, its Region). The 16 bytes are two hashes of all of
/// them, one per half of the UUID, and the version and variant bits are the renderer's.
#[must_use]
pub fn minted_uuid(domain: &[u8; 8], sequence: u64, now: SystemTime, parts: &[&str]) -> String {
    let bytes: [u8; 16] = minted_bytes(domain, sequence, now, parts, 0)
        .try_into()
        .unwrap_or_default();
    ::service_common::uuid_from_random_bytes(bytes)
}

/// `digits` lowercase hexadecimal characters minted for one resource of a given kind,
/// from the same derivation [`minted_uuid`] uses.
///
/// Several of this API's identifiers are not UUIDs but a fixed count of hexadecimal
/// characters behind a per-resource prefix (a tenant's `TenantId`), so the bytes are
/// minted the same way and rendered as that many digits: unique across the resources
/// of its kind, because the minting store's monotonic sequence number is part of what
/// they are derived from, and stable for as long as the resource holds the id.
#[must_use]
pub fn minted_hex(
    domain: &[u8; 8],
    sequence: u64,
    now: SystemTime,
    parts: &[&str],
    digits: usize,
) -> String {
    // Two hexadecimal characters per byte, and one more byte than that when an odd
    // count is asked for, so the rendered text is never short of `digits`.
    let mut hex: String = minted_bytes(domain, sequence, now, parts, digits.div_ceil(2))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    hex.truncate(digits);
    hex
}

/// The characters a label of a [`minted_label_pair`] identifier is drawn from: the
/// lowercase alphanumerics, which is what the labels of the recorded form carry.
const LABEL_ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// How many characters each of the two labels of a [`minted_label_pair`] identifier
/// carries. Well inside the 63 a DNS label admits, so the whole id stays a usable host
/// label whichever address it is placed in.
const LABEL_PAIR_WIDTH: usize = 13;

/// A pair of dot-separated alphanumeric labels — `<label>.<label>` — minted for one
/// resource of a given kind, from the same derivation [`minted_uuid`] uses.
///
/// This is the form SES hands a multi-region endpoint's `EndpointId` back in: TWO labels
/// of lowercase alphanumeric text with a single dot between them, and not a dashed
/// hexadecimal UUID, a bare run of hexadecimal characters or a prefixed token. A client
/// that validates the id, or places it where the endpoint it names is addressed, reads
/// that form, so the id is rendered in it here rather than in whatever form the store's
/// other opaque identifiers take.
///
/// The arguments are [`minted_uuid`]'s: `domain` keeps the id spaces apart, `sequence` is
/// the minting store's monotonic counter for the kind, `now` is the instant of the mint
/// and `parts` is whatever else names the resource — so two resources minted in the same
/// instant still differ, and the id is stable for as long as its resource holds it.
#[must_use]
pub fn minted_label_pair(
    domain: &[u8; 8],
    sequence: u64,
    now: SystemTime,
    parts: &[&str],
) -> String {
    let characters = 2 * LABEL_PAIR_WIDTH;
    let bytes = minted_bytes(domain, sequence, now, parts, characters);
    let mut value = String::with_capacity(characters + 1);
    for (index, byte) in bytes.iter().take(characters).enumerate() {
        if index == LABEL_PAIR_WIDTH {
            value.push('.');
        }
        // The character's position is part of what picks it, so a byte that repeats
        // inside the run does not repeat its character.
        let offset = usize::from(*byte) + index;
        value.push(char::from(LABEL_ALPHABET[offset % LABEL_ALPHABET.len()]));
    }
    value
}

/// The bytes an identifier of one resource is minted from: at least `wanted` of them,
/// rounded up to the 8-byte hash the derivation produces at a time, and 16 for the
/// caller that asks for none (the UUID's own length).
///
/// Each 8-byte run is one hash of the id's domain, whatever else names the resource,
/// the instant of the mint and the minting store's sequence number, salted by the run's
/// own index so two runs of one id differ.
fn minted_bytes(
    domain: &[u8; 8],
    sequence: u64,
    now: SystemTime,
    parts: &[&str],
    wanted: usize,
) -> Vec<u8> {
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let runs = wanted.div_ceil(8).max(2);
    let mut bytes = Vec::with_capacity(runs * 8);
    for run in 0..runs {
        let mut hasher = DefaultHasher::new();
        // The first run carries no domain salt, so the UUIDs this minted before the
        // hexadecimal form existed are minted unchanged.
        let salt = match run {
            0 => 0,
            _ => u64::from_be_bytes(*domain).wrapping_add(run as u64 - 1),
        };
        salt.hash(&mut hasher);
        for part in parts {
            part.hash(&mut hasher);
        }
        nanos.hash(&mut hasher);
        sequence.hash(&mut hasher);
        bytes.extend_from_slice(&hasher.finish().to_be_bytes());
    }
    bytes
}
