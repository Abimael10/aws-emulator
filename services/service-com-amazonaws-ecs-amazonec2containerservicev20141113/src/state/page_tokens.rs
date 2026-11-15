// SPDX-License-Identifier: CC0-1.0

//! The continuation token a paginated read of this service hands out, and how it tells one
//! it MINTED from one a caller assembled.
//!
//! Every paginated operation here documents its token the same way — `nextToken` "should be
//! treated as an opaque identifier that is only used to retrieve the next items in a list
//! and not for other programmatic purposes" — and an opaque identifier is exactly what this
//! module makes it. Two properties follow, and a listing needs both:
//!
//! * the token discloses no record of this store. A cursor spelled as the ARN or the name
//!   the next page starts at puts a resource identifier into a value clients log, compare,
//!   cache and route on, and makes the token's FORMAT observable — so a client comes to
//!   depend on it and any later change to how a listing walks its set breaks resumption
//!   silently;
//! * a token this store did not mint for THIS listing names no position at all, so it is
//!   refused by the operation that was handed it rather than silently read as some insertion
//!   point in the sequence. That covers a value a caller built, one another store minted
//!   (another emulator process, a restarted one), and one minted for another operation or
//!   another listing scope — a cursor spelled as a plain ARN is portable across the
//!   clusters, services and Regions whose listings happen to hold it, which no real
//!   continuation token is.
//!
//! The authenticator over the cursor is the workspace's one reading of a page token
//! ([`::service_common::page_tokens::PageTokenKey`]), under a secret this store draws from
//! the system's randomness when it is created and never reveals. What this module adds is
//! only what is this service's own: the SCOPE a token is bound to, and the hexadecimal
//! spelling the authenticated bytes travel as.
//!
//! WHICH refusal a token this listing never minted earns stays the OPERATION's own: the
//! class and the sentence each paginated read answers with are properties of that operation
//! (see [`super::INVALID_NEXT_TOKEN_MESSAGE`]), and this module only answers whether the
//! value names a position.

use super::*;

pub use ::service_common::page_tokens::PageTokenKey;

/// What separates a token's scope from the cursor inside it: a control character, which no
/// cluster name, service name, ARN, family name or setting name a scope or a cursor of this
/// service is built from can contain.
const CURSOR_SEPARATOR: char = '\u{1e}';

/// What separates the parts of a scope from each other, for the same reason.
const SCOPE_SEPARATOR: char = '\u{1f}';

/// The scope a listing's tokens are bound to: the operation that minted them, then the
/// parts of the request that decide WHICH set is being walked.
///
/// A token is honoured only against the scope it was minted for, so the continuation of one
/// listing can never resume another: `within` names the request's own scope as the store
/// keys it (the cluster of a service listing, the cluster and service of a deployment
/// listing, the family prefix of a revision listing).
#[must_use]
pub fn listing_scope(operation: &str, within: &[&str]) -> String {
    let mut scope = String::from(operation);
    for part in within {
        scope.push(SCOPE_SEPARATOR);
        scope.push_str(part);
    }
    scope
}

/// The `nextToken` a listing in `scope` hands back for `cursor`, the position the next page
/// resumes from.
///
/// Hexadecimal, so the token is one word of URL- and log-safe text whatever bytes the
/// authenticator holds.
#[must_use]
pub fn encode_page_token(key: &PageTokenKey, scope: &str, cursor: &str) -> String {
    ::hex::encode(key.mint(&format!("{scope}{CURSOR_SEPARATOR}{cursor}")))
}

/// The position `token` names for a listing in `scope`, or `None` for every value this
/// store did not mint for that scope: one that is not the hexadecimal of a minted token,
/// one whose authenticator does not match the cursor it travels with, and one minted for
/// another operation or another listing scope.
#[must_use]
pub fn decode_page_token(key: &PageTokenKey, scope: &str, token: &str) -> Option<String> {
    let bytes = ::hex::decode(token).ok()?;
    let minted = key.cursor_of(&bytes)?;
    let (minted_scope, cursor) = minted.split_once(CURSOR_SEPARATOR)?;
    (minted_scope == scope).then(|| cursor.to_string())
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The secret THIS store's page tokens are authenticated under.
    ///
    /// Handed back by value (the key is `Copy`), so a paginated read takes it off the store
    /// and goes on to borrow the records it is about to report in the same call.
    pub fn page_token_key(&self) -> PageTokenKey {
        self.page_token_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor round-trips under the key and the scope that minted it, and under nothing
    /// else: another store's key, another operation, another listing scope, and any value a
    /// caller could assemble name no position at all.
    #[test]
    fn a_cursor_round_trips_only_under_its_own_key_and_scope() {
        let key = PageTokenKey::new();
        let another_store = PageTokenKey::new();
        let scope = listing_scope("ListServices", &["production"]);
        let other_scopes = [
            listing_scope("ListServices", &["staging"]),
            listing_scope("ListServices", &[]),
            listing_scope("ListTasks", &["production"]),
            listing_scope("ListServices", &["production", "web"]),
        ];

        // Cursors spanning the positions a listing of this service names: a short name, a
        // full ARN, a composite position, one holding the separators themselves, and the
        // empty position.
        for cursor in [
            "",
            "web",
            "arn:aws:ecs:us-east-1:000000000000:service/production/web",
            "family:12",
            "a\u{1f}b\u{1e}c",
        ] {
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
            assert!(
                !token.contains(cursor) || cursor.is_empty(),
                "case {cursor:?}: the token must not carry the cursor in the clear, got \
                 {token:?}"
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

        // Values a caller could assemble, none of which this store minted for this scope.
        let minted = encode_page_token(&key, &scope, "web");
        for (case, forged) in [
            ("a plain name", "web".to_string()),
            (
                "a resource ARN",
                "arn:aws:ecs:us-east-1:000000000000:service/production/web".to_string(),
            ),
            ("empty", String::new()),
            ("not hexadecimal", "zzzz".to_string()),
            (
                "a minted token with its last byte dropped",
                minted[..minted.len() - 2].to_string(),
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
                "case {case:?}: a value this store did not mint must name no position"
            );
        }
    }
}
