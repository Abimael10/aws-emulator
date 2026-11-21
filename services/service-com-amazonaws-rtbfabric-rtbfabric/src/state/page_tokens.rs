// SPDX-License-Identifier: CC0-1.0

//! The ONE encoding this service's paginated reads hand a `nextToken` out in, and read
//! one back from.
//!
//! Every listing of this service walks its records in a total order and names the page
//! boundary with a cursor, so the token codec is stated once here rather than once per
//! listing: two listings then cannot drift apart about what a token means, and a token
//! one walk issued is never resumed from by another, because the walk that issued it
//! travels inside it — the operation, and for a listing addressed to a parent resource
//! the ids of the resources it was scoped to ([`Walk`]).
//!
//! The position a `nextToken` names travels INSIDE it, so what stops a caller from
//! naming a position no listing offered is an authenticator over those bytes, computed
//! under a secret the store draws from the system's randomness and never reveals: the
//! workspace's one reading of a page token
//! ([`::service_common::page_tokens::PageTokenKey`], held on this service's store). This
//! module adds only what is this service's own — the walk a token is bound to, the
//! issuing instant its documented span is counted from, and the hexadecimal spelling the
//! authenticated bytes travel as.

pub use ::service_common::page_tokens::PageTokenKey;

/// How long a `nextToken` a listing issued stays usable. The `nextToken` member's own
/// documentation states that each pagination token expires after 24 hours, so this span
/// is the service's rather than the emulator's choice.
pub const TOKEN_LIFETIME: ::std::time::Duration = ::std::time::Duration::from_secs(24 * 60 * 60);

/// Separates the three parts the authenticated bytes carry. A unit separator occurs in
/// none of them: the scope is an operation name (ASCII letters) and resource ids, a
/// resource id this service mints is `[a-z0-9-]` after its prefix, and the instant is
/// decimal digits.
const TOKEN_SEPARATOR: char = '\u{1f}';

/// Separates the ids a parent-scoped walk is scoped to from the operation that issued
/// it, inside the scope. A record separator occurs in no id this service's models admit
/// — every id is a prefix followed by `[a-z0-9-]`, which boundary validation enforces
/// before a handler runs — so one walk's scope can never be read as another's.
const SCOPE_SEPARATOR: char = '\u{1e}';

/// The longest `nextToken` a listing of this service can ever have issued: hex doubles
/// the authenticator ahead of the bytes it signs, the scope (the operation name, at most
/// 64 characters, and the ids the walk is scoped to with their separators — at most a
/// `GatewayId` of 32 and a `LinkId` of 30, which is what `ListLinkRoutingRules` is
/// addressed by), the cursor (a `GatewayId` is at most 32, the longest id any listing
/// pages by), the instant in whole seconds (at most 20 digits) and the two separators.
/// The model bounds `nextToken`'s length nowhere, so this is what bounds the work a
/// caller's token can cost: anything longer is refused before it is decoded.
pub const MAX_TOKEN_LENGTH: usize = 2
    * (::service_common::page_tokens::AUTHENTICATOR_BYTES + 64 + (32 + 30 + 2) + 32 + 20 + 2);

/// Which walk a `nextToken` belongs to, as it travels inside the token.
///
/// A cursor is an index into ONE enumeration, so the token has to say which: the
/// operation that issued it, and — where the listing is addressed to a parent resource
/// and so walks only that parent's records — the ids of the resources the walk was
/// scoped to. Resuming is then refused for every walk other than the one that handed
/// the token out, rather than reading one enumeration's cursor as another's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walk(String);

impl Walk {
    /// A listing of every record of one kind this service holds: nothing narrows the
    /// enumeration, so the operation alone identifies the walk.
    pub fn of(operation: &str) -> Self {
        Self(operation.to_string())
    }

    /// A listing addressed to parent resources, which walks only the records those
    /// parents own: `within` is their ids, in the order the request path states them,
    /// so a cursor into one parent's enumeration is never read as a cursor into
    /// another's.
    pub fn within(operation: &str, within: &[&str]) -> Self {
        let mut scope = operation.to_string();
        for id in within {
            scope.push(SCOPE_SEPARATOR);
            scope.push_str(id);
        }
        Self(scope)
    }
}

/// Why a `nextToken` could not be resumed from.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenRefusal {
    /// Not a token the addressed walk handed out.
    Unrecognised,
    /// The addressed walk's own token, but issued more than [`TOKEN_LIFETIME`] ago.
    Expired,
}

impl TokenRefusal {
    /// What the refusal says to the caller, who has to fix the request from this
    /// text alone: the two cases ask for different fixes — correct the client, or
    /// restart the walk — so they do not share one message.
    pub fn message(&self, operation: &str) -> String {
        match self {
            Self::Unrecognised => {
                format!("nextToken is not a pagination token {operation} issued")
            }
            Self::Expired => format!(
                "nextToken has expired: a {operation} pagination token expires {} hours after it is issued",
                TOKEN_LIFETIME.as_secs() / 3_600,
            ),
        }
    }
}

/// Render a page cursor as the opaque `nextToken` the next request replays.
/// `nextToken` is modelled as an opaque string, so the authenticated bytes are
/// rendered in hex, an alphabet that survives any transport. Two things travel with
/// the cursor: the [`Walk`] that issued it, so a token minted by one walk is not
/// resumed from by another, and the issuing instant, because the token's usable span
/// is counted from when it was handed out. `key` is the store's own secret, so no
/// caller can assemble a token this store did not hand out.
pub fn encode_next_token(
    key: &PageTokenKey,
    walk: &Walk,
    cursor: &str,
    issued_at: ::std::time::SystemTime,
) -> String {
    let seconds = issued_at
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let scope = &walk.0;
    hex::encode(key.mint(&format!(
        "{scope}{TOKEN_SEPARATOR}{cursor}{TOKEN_SEPARATOR}{seconds}"
    )))
}

/// Read a cursor back out of a `nextToken` this store issued for `walk`, or say why it
/// cannot be resumed from: a value that is not the hex of bytes `key` authenticated — a
/// token a caller assembled, or one another store minted — and a token minted for
/// another walk are alike not this walk's to resume from.
pub fn decode_next_token(
    key: &PageTokenKey,
    walk: &Walk,
    token: &str,
    now: ::std::time::SystemTime,
) -> Result<String, TokenRefusal> {
    if token.len() > MAX_TOKEN_LENGTH {
        return Err(TokenRefusal::Unrecognised);
    }
    let decoded = hex::decode(token)
        .ok()
        .and_then(|bytes| key.cursor_of(&bytes))
        .ok_or(TokenRefusal::Unrecognised)?;
    let mut parts = decoded.split(TOKEN_SEPARATOR);
    let (Some(scope), Some(cursor), Some(seconds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenRefusal::Unrecognised);
    };
    if scope != walk.0 || cursor.is_empty() {
        return Err(TokenRefusal::Unrecognised);
    }
    let issued_at = seconds
        .parse()
        .ok()
        .map(::std::time::Duration::from_secs)
        .and_then(|since_epoch| ::std::time::UNIX_EPOCH.checked_add(since_epoch))
        .ok_or(TokenRefusal::Unrecognised)?;
    let expires_at = issued_at
        .checked_add(TOKEN_LIFETIME)
        .ok_or(TokenRefusal::Unrecognised)?;
    if now >= expires_at {
        return Err(TokenRefusal::Expired);
    }
    Ok(cursor.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        decode_next_token, encode_next_token, PageTokenKey, TokenRefusal, Walk, MAX_TOKEN_LENGTH,
        TOKEN_LIFETIME,
    };
    use ::std::time::{Duration, SystemTime};

    /// The operation a token is scoped to, as a handler passes it.
    const OPERATION: &str = "ListRequesterGateways";

    /// A cursor survives the token it is rendered as, nothing else is read as a
    /// cursor, and a token past its documented span is refused as expired rather
    /// than as malformed: the token is this service's own encoding, so its
    /// rules are pinned here beside it.
    #[test]
    fn a_cursor_round_trips_through_its_token_until_the_token_expires() {
        let issued_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
        let key = PageTokenKey::new();
        let walk = Walk::of(OPERATION);

        for (case, cursor) in [
            ("a minted id", "rtb-gw-0000000000000001"),
            ("the shortest id the pattern allows", "rtb-gw-a"),
            ("every character class the pattern allows", "rtb-gw-a9-z"),
        ] {
            let token = encode_next_token(&key, &walk, cursor, issued_at);
            assert!(
                token.len() <= MAX_TOKEN_LENGTH,
                "case {case:?}: a token this operation issues must fit the length it accepts, got {token:?}"
            );
            assert_eq!(
                decode_next_token(&key, &walk, &token, issued_at).as_deref(),
                Ok(cursor),
                "case {case:?}: the cursor must survive its token {token:?}"
            );
            assert_eq!(
                decode_next_token(
                    &key,
                    &walk,
                    &token,
                    issued_at + TOKEN_LIFETIME - Duration::from_secs(1)
                )
                .as_deref(),
                Ok(cursor),
                "case {case:?}: a token inside its span is still usable"
            );
            assert_eq!(
                decode_next_token(&key, &walk, &token, issued_at + TOKEN_LIFETIME),
                Err(TokenRefusal::Expired),
                "case {case:?}: a token at the end of its span is expired, not malformed"
            );
            assert_eq!(
                decode_next_token(&key, &Walk::of("ListResponderGateways"), &token, issued_at),
                Err(TokenRefusal::Unrecognised),
                "case {case:?}: another listing's walk must not resume from this token"
            );
            assert_eq!(
                decode_next_token(&PageTokenKey::new(), &walk, &token, issued_at),
                Err(TokenRefusal::Unrecognised),
                "case {case:?}: another store never issued this token, so it must not resume from it"
            );
        }

        for (case, token) in [
            ("not hex at all", "not-a-token".to_string()),
            ("an odd number of hex digits", "abc".to_string()),
            ("hex carrying no separator", "6162".to_string()),
            (
                "hex carrying only two parts",
                hex::encode(format!("{OPERATION}\u{1f}rtb-gw-a")),
            ),
            (
                "hex whose instant is not a number",
                hex::encode(format!("{OPERATION}\u{1f}rtb-gw-a\u{1f}now")),
            ),
            (
                "hex carrying an empty cursor",
                hex::encode(format!("{OPERATION}\u{1f}\u{1f}0")),
            ),
            (
                "the shape of a token, all zero",
                hex::encode(vec![
                    0_u8;
                    ::service_common::page_tokens::AUTHENTICATOR_BYTES + 8
                ]),
            ),
            (
                "longer than any token this operation issues",
                "61".repeat(MAX_TOKEN_LENGTH),
            ),
        ] {
            assert_eq!(
                decode_next_token(&key, &walk, &token, issued_at),
                Err(TokenRefusal::Unrecognised),
                "case {case:?}: {token:?} is not a token this operation issued"
            );
        }
    }

    /// A cursor indexes into ONE enumeration, so only the walk that handed the token
    /// out resumes from it: the operation AND the parents the walk was scoped to have
    /// to agree, whatever the shape of the scope, or one parent's cursor would be read
    /// as an index into another's records.
    #[test]
    fn only_the_walk_that_issued_a_token_resumes_from_it() {
        let issued_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600);
        let key = PageTokenKey::new();
        let cursor = "link-0000000000000001";

        // Each row is the walk that issues the token, then the walks that must refuse
        // it: a different parent, a longer or shorter scope, a different operation on
        // the same parent.
        let rows: &[(&str, Walk, &[Walk])] = &[
            (
                "a walk over the whole service",
                Walk::of("ListRequesterGateways"),
                &[
                    Walk::of("ListResponderGateways"),
                    Walk::within("ListRequesterGateways", &["rtb-gw-0000000000000001"]),
                ],
            ),
            (
                "a walk scoped to one parent",
                Walk::within("ListLinks", &["rtb-gw-0000000000000001"]),
                &[
                    Walk::within("ListLinks", &["rtb-gw-0000000000000002"]),
                    Walk::within("ListLinks", &["rtb-gw-000000000000000"]),
                    Walk::within(
                        "ListLinks",
                        &["rtb-gw-0000000000000001", "link-0000000000000001"],
                    ),
                    Walk::within("ListCertificateAssociations", &["rtb-gw-0000000000000001"]),
                    Walk::of("ListLinks"),
                ],
            ),
            (
                "a walk scoped to two parents",
                Walk::within(
                    "ListLinkRoutingRules",
                    &["rtb-gw-0000000000000001", "link-0000000000000001"],
                ),
                &[
                    Walk::within(
                        "ListLinkRoutingRules",
                        &["rtb-gw-0000000000000001", "link-0000000000000002"],
                    ),
                    Walk::within(
                        "ListLinkRoutingRules",
                        &["rtb-gw-0000000000000002", "link-0000000000000001"],
                    ),
                    Walk::within(
                        "ListLinkRoutingRules",
                        &["link-0000000000000001", "rtb-gw-0000000000000001"],
                    ),
                    Walk::within("ListLinkRoutingRules", &["rtb-gw-0000000000000001"]),
                ],
            ),
        ];

        for (case, issuer, strangers) in rows {
            let token = encode_next_token(&key, issuer, cursor, issued_at);
            assert!(
                token.len() <= MAX_TOKEN_LENGTH,
                "case {case:?}: a token a walk issues must fit the length it accepts, got {token:?}"
            );
            assert_eq!(
                decode_next_token(&key, issuer, &token, issued_at).as_deref(),
                Ok(cursor),
                "case {case:?}: the issuing walk must resume from its own token {token:?}"
            );
            for stranger in *strangers {
                assert_eq!(
                    decode_next_token(&key, stranger, &token, issued_at),
                    Err(TokenRefusal::Unrecognised),
                    "case {case:?}: {stranger:?} never issued {token:?}, so it must not resume from it"
                );
            }
        }
    }
}
