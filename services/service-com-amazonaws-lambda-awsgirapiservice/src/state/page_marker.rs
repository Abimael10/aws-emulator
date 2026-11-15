// SPDX-License-Identifier: CC0-1.0

//! The SPELLING a `Marker` this store issued has on the wire, and the reading that tells
//! one it issued from one a caller assembled.
//!
//! A `Marker` is "the pagination token that's returned by a previous request", so what it
//! names is a place in ONE listing's order and nothing else: it is not any of the
//! identifiers this API addresses a resource by, and a caller cannot act on it except by
//! handing it back. The position therefore travels INSIDE the token, behind an
//! authenticator over it computed under the secret the store draws at construction
//! ([`super::PageTokenKey`]) — so the only values a listing continues from are the ones it
//! issued, and a token another store minted (another emulator process, a restarted one) is
//! refused for the same reason.
//!
//! [`super::PageTokenKey`] hands back BYTES and takes bytes, leaving how a service renders
//! them to the service; this module is that rendering for this service, in one place, so
//! two paginated listings of it cannot spell their tokens two ways. The bytes are rendered
//! hexadecimal — one fixed alphabet, needing no escaping in a query string — so a token
//! this service minted reads the same whatever position it carries.
//!
//! A listing refuses a value [`marker_cursor`] answers `None` for with the sentence that
//! listing words for a marker it did not issue (`super::INVALID_MARKER_MESSAGE`,
//! `super::INVALID_PAGINATION_KEY_MESSAGE`).

use super::PageTokenKey;

/// The `NextMarker` a page whose listing ended at `cursor` hands back: the position it
/// ended on, carried inside a token this store minted for it.
#[must_use]
pub fn minted_marker(key: &PageTokenKey, cursor: &str) -> String {
    ::hex::encode(key.mint(cursor))
}

/// The position `marker` names, or `None` for any value this store did not mint — a value
/// of the wrong spelling, and one whose authenticator does not match the position it
/// travels with.
#[must_use]
pub fn marker_cursor(key: &PageTokenKey, marker: &str) -> Option<String> {
    let token = ::hex::decode(marker).ok()?;
    key.cursor_of(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker round-trips under the key that minted it, names no position under another
    /// store's key, and is never the position it carries.
    ///
    /// The rows are the positions this service's listings page by: a layer name, a
    /// qualifier, a version label and the shortest and longest of them.
    #[test]
    fn a_marker_round_trips_and_only_under_the_key_that_minted_it() {
        let key = PageTokenKey::new();
        let other = PageTokenKey::new();
        for cursor in ["a", "layer-1", "$LATEST", "Blue_green-2", &"n".repeat(140)] {
            let marker = minted_marker(&key, cursor);
            assert_eq!(
                marker_cursor(&key, &marker).as_deref(),
                Some(cursor),
                "case {cursor:?}: expected the marker to name the position it was minted \
                 for, got {:?}",
                marker_cursor(&key, &marker)
            );
            assert!(
                marker != cursor && marker.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "case {cursor:?}: a marker is a hexadecimal token of this store's own, \
                 never the position it continues after, got {marker:?}"
            );
            for (case, value) in [
                ("another store's key", minted_marker(&other, cursor)),
                ("the bare position", cursor.to_string()),
                (
                    "a token shaped like a minted one",
                    ::hex::encode(
                        [
                            &[0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES][..],
                            cursor.as_bytes(),
                        ]
                        .concat(),
                    ),
                ),
                ("not a hexadecimal token at all", format!("{marker}!!")),
                ("an empty value", String::new()),
            ] {
                assert_eq!(
                    marker_cursor(&key, &value),
                    None,
                    "case {cursor:?} / {case}: expected no position from {value:?}"
                );
            }
        }
    }
}
