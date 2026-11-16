// SPDX-License-Identifier: CC0-1.0

//! The ONE encoding this service's paginated reads hand a `nextToken` out in, and read
//! one back from.
//!
//! Every listing of this service walks its records in a total order and names the page
//! boundary with a cursor, so the token codec is stated once here rather than once per
//! listing: two listings then cannot drift apart about what a token means, and a token
//! one of them issued is never resumed from by another, because the issuing operation
//! travels inside it.

/// How long a `nextToken` a listing issued stays usable. The `nextToken` member's own
/// documentation states that each pagination token expires after 24 hours, so this span
/// is the service's rather than the emulator's choice.
pub const TOKEN_LIFETIME: ::std::time::Duration = ::std::time::Duration::from_secs(24 * 60 * 60);

/// Separates the three parts a token carries. A unit separator occurs in none of
/// them: the operation name is ASCII letters, a resource id this service mints is
/// `[a-z0-9-]` after its prefix, and the instant is decimal digits.
const TOKEN_SEPARATOR: char = '\u{1f}';

/// The longest `nextToken` a listing of this service can ever have issued: hex doubles
/// the operation name (at most 64 characters), the cursor (a `GatewayId` is at most 32,
/// the longest id any listing pages by), the instant in whole seconds (at most 20
/// digits) and the two separators. The model bounds `nextToken`'s length nowhere, so
/// this is what bounds the work a caller's token can cost: anything longer is refused
/// before it is decoded.
pub const MAX_TOKEN_LENGTH: usize = 2 * (64 + 32 + 20 + 2);

/// Why a `nextToken` could not be resumed from.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenRefusal {
    /// Not a token this operation handed out.
    Unrecognised,
    /// This operation's own token, but issued more than [`TOKEN_LIFETIME`] ago.
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
/// `nextToken` is modelled as an opaque string, so the cursor is rendered in
/// hex, an alphabet that survives any transport. Two things travel with it: the
/// operation that issued it, so a token minted by one listing is not resumed
/// from by another, and the issuing instant, because the token's usable span is
/// counted from when it was handed out.
pub fn encode_next_token(
    operation: &str,
    cursor: &str,
    issued_at: ::std::time::SystemTime,
) -> String {
    let seconds = issued_at
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    hex::encode(format!(
        "{operation}{TOKEN_SEPARATOR}{cursor}{TOKEN_SEPARATOR}{seconds}"
    ))
}

/// Read a cursor back out of a `nextToken` `operation` issued, or say why it
/// cannot be resumed from.
pub fn decode_next_token(
    operation: &str,
    token: &str,
    now: ::std::time::SystemTime,
) -> Result<String, TokenRefusal> {
    if token.len() > MAX_TOKEN_LENGTH {
        return Err(TokenRefusal::Unrecognised);
    }
    let decoded = hex::decode(token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or(TokenRefusal::Unrecognised)?;
    let mut parts = decoded.split(TOKEN_SEPARATOR);
    let (Some(scope), Some(cursor), Some(seconds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenRefusal::Unrecognised);
    };
    if scope != operation || cursor.is_empty() {
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
        decode_next_token, encode_next_token, TokenRefusal, MAX_TOKEN_LENGTH, TOKEN_LIFETIME,
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

        for (case, cursor) in [
            ("a minted id", "rtb-gw-0000000000000001"),
            ("the shortest id the pattern allows", "rtb-gw-a"),
            ("every character class the pattern allows", "rtb-gw-a9-z"),
        ] {
            let token = encode_next_token(OPERATION, cursor, issued_at);
            assert!(
                token.len() <= MAX_TOKEN_LENGTH,
                "case {case:?}: a token this operation issues must fit the length it accepts, got {token:?}"
            );
            assert_eq!(
                decode_next_token(OPERATION, &token, issued_at).as_deref(),
                Ok(cursor),
                "case {case:?}: the cursor must survive its token {token:?}"
            );
            assert_eq!(
                decode_next_token(
                    OPERATION,
                    &token,
                    issued_at + TOKEN_LIFETIME - Duration::from_secs(1)
                )
                .as_deref(),
                Ok(cursor),
                "case {case:?}: a token inside its span is still usable"
            );
            assert_eq!(
                decode_next_token(OPERATION, &token, issued_at + TOKEN_LIFETIME),
                Err(TokenRefusal::Expired),
                "case {case:?}: a token at the end of its span is expired, not malformed"
            );
            assert_eq!(
                decode_next_token("ListResponderGateways", &token, issued_at),
                Err(TokenRefusal::Unrecognised),
                "case {case:?}: another listing's walk must not resume from this token"
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
                "longer than any token this operation issues",
                "61".repeat(MAX_TOKEN_LENGTH),
            ),
        ] {
            assert_eq!(
                decode_next_token(OPERATION, &token, issued_at),
                Err(TokenRefusal::Unrecognised),
                "case {case:?}: {token:?} is not a token this operation issued"
            );
        }
    }
}
