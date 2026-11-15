// SPDX-License-Identifier: CC0-1.0

//! The identifiers this service MINTS in UUID form, and the one function that writes
//! them.
//!
//! Several of this API's identifiers — the multi-region endpoint's `EndpointId`, an
//! export job's `JobId`, an import job's `JobId` — carry no modelled pattern and are
//! handed back by SES as UUIDs. A UUID is not merely "32 hexadecimal digits in dashed
//! 8-4-4-4-12 groups": its layout also fixes the version and the variant bits, and a
//! client that parses or round-trips one through a UUID type rejects a value whose
//! groups are the right length but whose bits say it is no UUID. So an identifier this
//! store mints in that form is RENDERED as a UUID rather than assembled out of hex
//! digits here, by the maintained `uuid` crate through
//! [`::service_common::uuid_from_random_bytes`] — the one place in this project that
//! owns the version and variant bits and the group boundaries.
//!
//! The bytes stay this store's own: [`minted_uuid`] derives them from the minting
//! store's monotonic sequence number, the instant of the mint and whatever else names
//! the resource, so an id is unique across the resources of its kind and stable for as
//! long as the resource holds it (it is minted once, at create, and read back from the
//! record afterwards).

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
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut bytes = [0_u8; 16];
    for (half, salt) in [0_u64, u64::from_be_bytes(*domain)].into_iter().enumerate() {
        let mut hasher = DefaultHasher::new();
        salt.hash(&mut hasher);
        for part in parts {
            part.hash(&mut hasher);
        }
        nanos.hash(&mut hasher);
        sequence.hash(&mut hasher);
        bytes[half * 8..(half + 1) * 8].copy_from_slice(&hasher.finish().to_be_bytes());
    }
    ::service_common::uuid_from_random_bytes(bytes)
}
