// SPDX-License-Identifier: CC0-1.0

//! The continuation token a paginated listing of this service hands out, and how it tells
//! one it ISSUED from one a caller assembled.
//!
//! A `NextToken` is an OPAQUE string: the position it names travels inside it, so the
//! token carries nothing a client is meant to read and nothing a client can write. Two
//! properties follow, and both are what a listing needs:
//!
//! * the token discloses no stored record — a cursor spelled as the last NAME on the page
//!   puts catalog contents into a value clients log, compare and pass around; and
//! * a token this store did not mint names no position at all, so it is refused instead of
//!   silently restarting or skipping an enumeration. That covers a value a caller built, a
//!   value another store minted (another emulator process, a restarted one), and a token
//!   minted for a DIFFERENT listing or a different request scope — a cursor spelled as a
//!   plain name is portable across the databases, catalogs and Regions whose listings
//!   happen to hold that name, which no real continuation token is.
//!
//! The authenticator over the cursor is the workspace's one reading of a page token
//! ([`::service_common::page_tokens::PageTokenKey`]), under a secret this store draws from
//! the system's randomness when it is created and never reveals. This module adds only what
//! is this service's own: the SCOPE a token is bound to, and the hexadecimal spelling the
//! authenticated bytes travel as.

pub use ::service_common::page_tokens::PageTokenKey;

/// What separates a token's scope from the position inside it: a control character, which
/// no name, id or Region this service's scopes are built from can contain (every catalog,
/// database, table and job name is drawn from `NameString`, whose pattern admits no
/// character below `\u{0020}`).
const CURSOR_SEPARATOR: char = '\u{1e}';

/// What separates the parts of a scope from each other, for the same reason.
const SCOPE_SEPARATOR: char = '\u{1f}';

/// The scope a listing's tokens are bound to: the operation that minted them, then the
/// resource whose contents it was enumerating.
///
/// A token is honoured only against the scope it was minted for, so a continuation of one
/// listing can never resume another: `within` names the parts of the request that decide
/// WHICH set is being paged (the catalog and database of a table listing, the Region of a
/// job listing), each as the store keys it.
#[must_use]
pub fn listing_scope(operation: &str, within: &[&str]) -> String {
    let mut scope = String::from(operation);
    for part in within {
        scope.push(SCOPE_SEPARATOR);
        scope.push_str(part);
    }
    scope
}

/// The `NextToken` a listing in `scope` hands back for `cursor`, the position the next page
/// resumes from.
///
/// Hexadecimal, so the token is one word of URL- and log-safe text whatever bytes the
/// authenticator holds.
#[must_use]
pub fn encode_page_token(key: &PageTokenKey, scope: &str, cursor: &str) -> String {
    ::hex::encode(key.mint(&format!("{scope}{CURSOR_SEPARATOR}{cursor}")))
}

/// The position `token` names for a listing in `scope`, or `None` for any value this store
/// did not mint for that scope: a value that is not the hexadecimal of a minted token, one
/// whose authenticator does not match the cursor it travels with, and one minted for
/// another listing or another resource.
#[must_use]
pub fn decode_page_token(key: &PageTokenKey, scope: &str, token: &str) -> Option<String> {
    let bytes = ::hex::decode(token).ok()?;
    let minted = key.cursor_of(&bytes)?;
    let (minted_scope, cursor) = minted.split_once(CURSOR_SEPARATOR)?;
    (minted_scope == scope).then(|| cursor.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor round-trips under the key and the scope that minted it, and under nothing
    /// else: another scope, another store's key, and a value a caller assembled all name no
    /// position.
    #[test]
    fn a_cursor_round_trips_only_under_its_own_key_and_scope() {
        let key = PageTokenKey::new();
        let another_store = PageTokenKey::new();
        let scope = listing_scope("GetTables", &["123456789012", "sales"]);
        let other_scopes = [
            listing_scope("GetTables", &["123456789012", "marketing"]),
            listing_scope("GetTables", &["210987654321", "sales"]),
            listing_scope("GetTables", &["123456789012"]),
            listing_scope("ListJobs", &["123456789012", "sales"]),
        ];

        // Cursors spanning the positions a listing of this service names: a short name, a
        // long one, one holding the separators themselves, and the empty position.
        for cursor in ["", "a", "cloudfront_access_logs", "a\u{1f}b\u{1e}c"] {
            let token = encode_page_token(&key, &scope, cursor);
            assert_eq!(
                decode_page_token(&key, &scope, &token).as_deref(),
                Some(cursor),
                "case {cursor:?}: a minted token must name its own cursor"
            );
            assert_eq!(
                decode_page_token(&another_store, &scope, &token),
                None,
                "case {cursor:?}: another store's key must not decode this token"
            );
            for other in &other_scopes {
                assert_eq!(
                    decode_page_token(&key, other, &token),
                    None,
                    "case {cursor:?}: a token minted for {scope:?} must not decode in \
                     {other:?}"
                );
            }
        }

        // Values a caller could assemble, none of which this store minted.
        for (case, forged) in [
            ("a plain name", "cloudfront_access_logs".to_string()),
            ("empty", String::new()),
            ("not hexadecimal", "zzzz".to_string()),
            (
                "a minted token with its last byte dropped",
                {
                    let token = encode_page_token(&key, &scope, "a");
                    token[..token.len() - 2].to_string()
                },
            ),
            (
                "the shape of a token, all zero",
                ::hex::encode(vec![
                    0_u8;
                    ::service_common::page_tokens::AUTHENTICATOR_BYTES + 4
                ]),
            ),
        ] {
            assert_eq!(
                decode_page_token(&key, &scope, &forged),
                None,
                "case {case}: a value this store did not mint must name no position"
            );
        }
    }
}
