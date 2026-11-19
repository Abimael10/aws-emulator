// SPDX-License-Identifier: CC0-1.0

//! The continuation token every `@paginated` read of this service hands out, and the key
//! it is sealed under.
//!
//! A `NextToken` is documented as nothing but the value a client hands back — "[m]ake the
//! call again using the returned token to retrieve the next page"
//! (`smithy_query(query="operation",operation="SearchListings")`) — so the only two
//! properties it has are the ones this module gives it:
//!
//! * it DISCLOSES nothing. The position a page resumes at and the query it resumes belong
//!   to the service, not to the holder of the token, so neither reaches the wire in a form
//!   a client (or a proxy, a log, a cache key or a test fixture that forwards the token)
//!   can read back. What travels is one sealed blob.
//! * it is UNFORGEABLE. Only a value this store sealed names a position at all, so a
//!   caller cannot reach a page position it was never handed — a token assembled by hand,
//!   one another store minted, one from a restarted process — and every other value is
//!   refused with the modelled `ValidationException` instead of being served as a page.
//!
//! The cursor is sealed with AES-256-GCM under a key this store draws from the system's
//! randomness when it is created and never reveals ([`PageCursorKey`]), using `ring`'s own
//! implementation of the scheme and of the randomness; nothing cryptographic is composed
//! here. Sealing, rather than the authenticate-in-the-clear cursor of
//! [`::service_common::page_tokens`], is what this service's token needs: its cursor is
//! bound to the REQUEST that cut the page (the resolved locale, the caller's search text,
//! the filter set, the sort key and direction, or the resource whose sub-collection is
//! being paged), and that is caller content the token must not carry in readable form.
//!
//! The sealed bytes travel as standard base64, whose alphabet is exactly `NextToken`'s
//! `@pattern ^[a-zA-Z0-9+/=]+$`, and a token is a fixed 92 characters — far inside the
//! member's `@length(0..8192)` for every query, however long the request that cut it.

use ::ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ::ring::rand::{SecureRandom, SystemRandom};

/// The width of the sealing key: the 256 bits [`AES_256_GCM`] takes.
const KEY_BYTES: usize = 32;

/// How the position a page resumes at is laid out in a cursor: a `u64` big-endian, so
/// every position is the same width and the payload's length tells nothing apart.
const POSITION_BYTES: usize = 8;

/// The width of the query digest a cursor carries, which is SHA-256's output.
const DIGEST_BYTES: usize = 32;

/// The cursor a token seals: the position, then the digest of the query it belongs to.
const CURSOR_BYTES: usize = POSITION_BYTES + DIGEST_BYTES;

/// The string every query digest is taken over, so a digest from this codec collides with
/// no other digest this service computes.
const DIGEST_DOMAIN: &str = "com.amazonaws.marketplacediscovery#page-cursor\n";

/// The refusal for a `nextToken` that is not a cursor THIS store sealed: a value a caller
/// assembled, one another store (or a restarted process) sealed, or one whose bytes were
/// changed on the way back.
const NOT_MINTED: &str = "nextToken is not a pagination token issued by this service";

/// The refusal for a cursor this store sealed for a DIFFERENT query than the one it came
/// back on.
const OTHER_QUERY: &str =
    "nextToken was issued for a different request; page the same request or omit nextToken";

/// The refusal for a cursor that resumes past the end of the result set it was cut over.
const PAST_THE_END: &str = "nextToken refers to a position beyond the end of the result set";

/// The secret this store's pagination cursors are sealed under.
///
/// Drawn from the system's randomness when the store is created, held only in memory,
/// never derived from a request, never reported and never logged (its [`std::fmt::Debug`]
/// renders nothing of it). `Copy`, so a read can take the key off its store and go on to
/// borrow the records it is about to report in the same call.
#[derive(Clone, Copy)]
pub struct PageCursorKey([u8; KEY_BYTES]);

impl PageCursorKey {
    /// A fresh key from the system's random generator.
    ///
    /// Explicit at the call site rather than hidden behind a `Default`, because what this
    /// answers is new randomness and not a zero value.
    #[must_use]
    pub fn new() -> Self {
        let mut key = [0_u8; KEY_BYTES];
        SystemRandom::new()
            .fill(&mut key)
            .expect("the system random generator fills a page-cursor key");
        Self(key)
    }

    /// `cursor` sealed under this key: a fresh nonce, then the ciphertext and the
    /// authentication tag over it.
    ///
    /// INFALLIBLE, because a paginated read that has more results to report hands back a
    /// token: the only failures the library reports here are the system generator failing
    /// to produce a nonce and a key of the wrong width, and neither is a state this store
    /// can reach.
    fn seal(&self, cursor: &[u8]) -> Vec<u8> {
        let mut nonce = [0_u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .expect("the system random generator fills a page-cursor nonce");
        let mut sealed = cursor.to_vec();
        self.cipher()
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut sealed,
            )
            .expect("a page cursor seals under this store's key");
        let mut token = Vec::with_capacity(NONCE_LEN + sealed.len());
        token.extend_from_slice(&nonce);
        token.append(&mut sealed);
        token
    }

    /// The cursor `token` carries, or `None` for any value this key did not seal: one too
    /// short to carry a nonce and a tag, and one whose tag does not authenticate the
    /// ciphertext it travels with.
    ///
    /// The tag check is the library's, so a refusal tells the caller nothing about how
    /// close its value was.
    fn open(&self, token: &[u8]) -> Option<Vec<u8>> {
        let (nonce, sealed) = token.split_at_checked(NONCE_LEN)?;
        let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
        let mut sealed = sealed.to_vec();
        let opened = self
            .cipher()
            .open_in_place(nonce, Aad::empty(), &mut sealed)
            .ok()?;
        Some(opened.to_vec())
    }

    /// This key as the cipher's own key type. Built per use, which costs one key schedule
    /// and keeps [`PageCursorKey`] a plain `Copy` value a store can hold.
    fn cipher(&self) -> LessSafeKey {
        LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &self.0).expect("the key is one AES-256-GCM width"),
        )
    }
}

impl Default for PageCursorKey {
    fn default() -> Self {
        Self::new()
    }
}

/// Redacted: the key seals this store's page cursors, so it belongs in no log line and in
/// no debug dump of the state that holds it.
impl ::std::fmt::Debug for PageCursorKey {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter.write_str("PageCursorKey(..)")
    }
}

/// The digest a cursor names its query by: the query identity never reaches the wire, and
/// the digest itself travels inside the seal, so a holder of the token can neither read
/// the query back nor test a guess at it.
fn query_digest(query_identity: &str) -> [u8; DIGEST_BYTES] {
    let mut digested = ::ring::digest::Context::new(&::ring::digest::SHA256);
    digested.update(DIGEST_DOMAIN.as_bytes());
    digested.update(query_identity.as_bytes());
    let mut digest = [0_u8; DIGEST_BYTES];
    digest.copy_from_slice(digested.finish().as_ref());
    digest
}

/// The position field of a cursor: the offset as a fixed-width big-endian `u64`.
fn position_bytes(offset: usize) -> [u8; POSITION_BYTES] {
    u64::try_from(offset).unwrap_or(u64::MAX).to_be_bytes()
}

/// Mint the token that resumes `query_identity`'s result set at `offset`.
///
/// Base64's standard alphabet is exactly the `NextToken` pattern `^[a-zA-Z0-9+/=]+$`.
pub fn encode_page_token(key: &PageCursorKey, query_identity: &str, offset: usize) -> String {
    let mut cursor = Vec::with_capacity(CURSOR_BYTES);
    cursor.extend_from_slice(&position_bytes(offset));
    cursor.extend_from_slice(&query_digest(query_identity));
    ::aws_smithy_types::base64::encode(key.seal(&cursor))
}

/// Read back a token minted by [`encode_page_token`] for the same query.
///
/// The `Err` is the refusal message: a token this store did not seal, or one it sealed for
/// another query, is bad input rather than a reason to silently restart at page 1.
pub fn decode_page_token(
    key: &PageCursorKey,
    token: &str,
    query_identity: &str,
) -> Result<usize, String> {
    let sealed =
        ::aws_smithy_types::base64::decode(token).map_err(|_| NOT_MINTED.to_string())?;
    let cursor = key.open(&sealed).ok_or_else(|| NOT_MINTED.to_string())?;
    // A cursor of another width is one this codec never wrote, so it names no position
    // even though this store's key opened it.
    if cursor.len() != CURSOR_BYTES {
        return Err(NOT_MINTED.to_string());
    }
    let (position, digest) = cursor.split_at(POSITION_BYTES);
    let position: [u8; POSITION_BYTES] = position.try_into().map_err(|_| NOT_MINTED.to_string())?;
    if digest != query_digest(query_identity) {
        return Err(OTHER_QUERY.to_string());
    }
    usize::try_from(u64::from_be_bytes(position)).map_err(|_| NOT_MINTED.to_string())
}

/// One page of `items` plus the token for the next one, or the refusal message for a
/// `nextToken` this store did not seal for this query.
pub fn page<'a, T>(
    key: &PageCursorKey,
    items: &'a [T],
    next_token: Option<&str>,
    query_identity: &str,
    max_results: i32,
) -> Result<(&'a [T], Option<String>), String> {
    let offset = match next_token {
        Some(token) if !token.is_empty() => decode_page_token(key, token, query_identity)?,
        _ => 0,
    };
    if offset > items.len() {
        return Err(PAST_THE_END.to_string());
    }
    let page_size = usize::try_from(max_results).unwrap_or(1).max(1);
    let end = offset.saturating_add(page_size).min(items.len());
    let token = (end < items.len()).then(|| encode_page_token(key, query_identity, end));
    Ok((&items[offset..end], token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor round-trips under the key and the query that cut it, and no other value
    /// names a position: not one a caller assembled, not one another store sealed, not one
    /// this store sealed for another query, and not a changed byte of a real token.
    #[test]
    fn a_cursor_round_trips_only_under_its_own_key_and_query() {
        let key = PageCursorKey::new();
        let another_store = PageCursorKey::new();
        // Identities spanning the forms this service's paginated reads cut a page under:
        // the whole-catalog search, a keyword search, a filtered and sorted one, a
        // sub-collection of one resource, and an identity carrying the separators the
        // handlers write.
        let identities = [
            "SearchListings/en-US///RELEVANCE/DESCENDING",
            "SearchListings/en-US/cloud//RELEVANCE/DESCENDING",
            "SearchListings/fr/cloud/CATEGORY=c-1|c-2/AVERAGE_CUSTOMER_RATING/ASCENDING",
            "GetOfferTerms/en-US/offer-1234567890123456789012345",
            "ListFulfillmentOptions/ja/prod-1",
        ];
        for identity in identities {
            for offset in [0_usize, 1, 25, 4096, usize::from(u16::MAX)] {
                let token = encode_page_token(&key, identity, offset);
                assert_eq!(
                    decode_page_token(&key, &token, identity),
                    Ok(offset),
                    "case {identity:?} offset {offset}: a minted token must name its position"
                );
                assert_eq!(
                    decode_page_token(&another_store, &token, identity),
                    Err(NOT_MINTED.to_string()),
                    "case {identity:?} offset {offset}: another store must not read the cursor"
                );
                for other in identities.iter().filter(|other| **other != identity) {
                    assert_eq!(
                        decode_page_token(&key, &token, other),
                        Err(OTHER_QUERY.to_string()),
                        "case {identity:?} offset {offset}: must not resume query {other:?}"
                    );
                }

                // The token discloses neither the query it belongs to nor its position.
                // The position's DECIMAL rendering is checked only where it is wide enough
                // for a chance run of ciphertext bytes not to look like one.
                let sealed = ::aws_smithy_types::base64::decode(&token)
                    .expect("a minted token is base64");
                let decimal = offset.to_string();
                assert!(
                    !contains(&sealed, identity.as_bytes())
                        && !contains(&sealed, &position_bytes(offset))
                        && (decimal.len() < 3 || !contains(&sealed, decimal.as_bytes())),
                    "case {identity:?} offset {offset}: the token must carry no readable \
                     rendering of the query or the position, got {sealed:?}"
                );
                assert!(
                    !token.is_empty()
                        && token.len() <= 8192
                        && token
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte)),
                    "case {identity:?} offset {offset}: a token must be a NextToken, got \
                     {token:?}"
                );

                // Every single-byte change to a sealed token, plus one truncated and one
                // extended.
                for index in 0..sealed.len() {
                    let mut tampered = sealed.clone();
                    tampered[index] ^= 0x01;
                    assert_eq!(
                        decode_page_token(
                            &key,
                            &::aws_smithy_types::base64::encode(&tampered),
                            identity
                        ),
                        Err(NOT_MINTED.to_string()),
                        "case {identity:?} offset {offset}: byte {index} changed must not read"
                    );
                }
                for (case, forged) in [
                    ("truncated", sealed[..sealed.len() - 1].to_vec()),
                    ("extended", [sealed.as_slice(), b"x"].concat()),
                ] {
                    assert_eq!(
                        decode_page_token(
                            &key,
                            &::aws_smithy_types::base64::encode(&forged),
                            identity
                        ),
                        Err(NOT_MINTED.to_string()),
                        "case {identity:?} offset {offset}: a {case} token must not read"
                    );
                }
            }
        }

        // Values a caller writes by hand: the plaintext cursor the codec once emitted,
        // base64 of a query identity, an empty seal, and text that is not base64 at all.
        for (case, forged) in [
            ("plaintext offset and identity", {
                ::aws_smithy_types::base64::encode("1:SearchListings/en-US/cloud//RELEVANCE/DESCENDING")
            }),
            ("plaintext identity", {
                ::aws_smithy_types::base64::encode("SearchListings/en-US/cloud//RELEVANCE/DESCENDING")
            }),
            ("a nonce and nothing else", {
                ::aws_smithy_types::base64::encode(vec![0_u8; NONCE_LEN])
            }),
            ("zeroes of a token's width", {
                ::aws_smithy_types::base64::encode(vec![0_u8; NONCE_LEN + CURSOR_BYTES + 16])
            }),
            ("not base64", "not a token!".to_string()),
        ] {
            assert_eq!(
                decode_page_token(&key, &forged, identities[1]),
                Err(NOT_MINTED.to_string()),
                "case {case:?}: a value this store did not seal must name no position"
            );
        }
    }

    /// Two stores draw two different secrets, and the secret is in no rendering of the key.
    #[test]
    fn keys_differ_and_are_not_rendered() {
        let key = PageCursorKey::new();
        let another_store = PageCursorKey::new();
        assert_ne!(
            key.0, another_store.0,
            "two keys drawn from the system's randomness must differ"
        );
        assert_eq!(
            format!("{key:?}"),
            "PageCursorKey(..)",
            "the key must render redacted"
        );
    }

    /// Whether `haystack` carries `needle` anywhere in it.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
    }
}
