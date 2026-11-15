// SPDX-License-Identifier: CC0-1.0

//! The reading this plane makes of a USER's bearer token — the OAuth 2.0 token a caller
//! presents when it asks for a workload access token on behalf of a user.
//!
//! `GetWorkloadAccessTokenForJWT` is handed `userToken`, "the OAuth 2.0 token issued by the
//! user's identity provider", and its modelled `UnauthorizedException` (HTTP 401) is the one
//! thrown "when the JWT bearer token is invalid or not found for OAuth bearer token based
//! access". So the token is a value to be VALIDATED before anything is minted from it, never
//! a value to be trusted: a presented token the plane cannot accept earns that refusal, and
//! no workload access token is minted and no issuance recorded.
//!
//! What can be decided here, and what cannot:
//!
//! * the FORM is decided by the tree's one reading of a compact JWS
//!   ([`::service_common::jwt::read`]): three base64url segments, a JOSE header naming a
//!   signing algorithm, a JSON claim set — and not the UNSECURED form of RFC 7515 §6, which
//!   is a token no issuer signed. A value inside the modelled `@pattern` (three
//!   `.`-separated runs of base64url characters) can still be none of that, which is why the
//!   refusal class is this one rather than `ValidationException`;
//! * the WINDOW is decided from the token's own claims ([`::service_common::jwt::CompactJws::outside_window`],
//!   RFC 7519 §4.1.4/§4.1.5): a token whose `exp` has passed, whose `nbf` has not yet
//!   arrived, or that names no expiry at all is not one this plane accepts;
//! * the SUBJECT the token speaks for is its `sub` claim (RFC 7519 §4.1.2), which is the
//!   user a token minted from it acts on behalf of. A token naming no subject identifies no
//!   user, so it is not a token this call can act on;
//! * the SIGNATURE cannot be verified: checking one needs the verification key the issuing
//!   identity provider publishes at its own key-set endpoint, and this emulator makes no
//!   outbound connection to fetch one. Nothing here reports a signature as verified, and a
//!   claim read out of a presented token is evidence for REFUSING it, never for granting
//!   anything beyond the user it names.

use std::time::SystemTime;

use ::service_common::jwt;

/// Why a presented user token is not one this plane accepts, with the sentence AWS refuses
/// each class with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTokenRefusal {
    /// The token's own validity window has run out: `now` is at or after its `exp`.
    Expired,
    /// The token is not one this plane can read and act on: not a compact JWS carrying a
    /// JSON claim set, one nothing signed, one outside its window for any other reason, or
    /// one naming no subject.
    Invalid,
}

/// The fixed text AWS refuses an EXPIRED bearer token with. It carries no request- or
/// state-derived part: the token's own instants are not quoted back, and neither is the
/// token.
pub const EXPIRED_USER_TOKEN_MESSAGE: &str = "Token has expired";

/// The fixed text AWS refuses a bearer token it cannot accept for any other reason with —
/// the class its own documentation words as the JWT bearer token being invalid. The
/// presented value is NOT quoted back: it is a bearer credential of the user, so a refusal
/// never echoes it.
pub const INVALID_USER_TOKEN_MESSAGE: &str = "Invalid JWT token";

impl UserTokenRefusal {
    /// The sentence this refusal is worded with.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Expired => EXPIRED_USER_TOKEN_MESSAGE,
            Self::Invalid => INVALID_USER_TOKEN_MESSAGE,
        }
    }
}

/// The user a presented token speaks for, when the token is one this plane accepts at `now`.
///
/// `Ok(subject)` is the token's `sub` claim — the user a workload access token minted from
/// this call acts on behalf of. `Err` is the class the call is refused as, which the handler
/// answers in its modelled `UnauthorizedException`; see the module docs for what is decided
/// and what is not.
pub fn user_of_presented_token(
    presented: &str,
    now: SystemTime,
) -> Result<String, UserTokenRefusal> {
    // A presentation prefix is not part of the token, so it is stripped before the token is
    // read — the one reading of that prefix in the tree.
    let token = jwt::read(jwt::without_bearer_scheme(presented))
        .map_err(|_defect| UserTokenRefusal::Invalid)?;
    if let Some(defect) = token.outside_window(now) {
        return Err(match defect {
            // The one class AWS words with its own sentence; every other way a window can
            // fail is the invalid-token class.
            jwt::WindowDefect::Expired => UserTokenRefusal::Expired,
            jwt::WindowDefect::NotYetValid | jwt::WindowDefect::NoExpiry => {
                UserTokenRefusal::Invalid
            }
        });
    }
    token
        .claim_str(SUBJECT_CLAIM)
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
        .map(str::to_string)
        .ok_or(UserTokenRefusal::Invalid)
}

/// The claim naming "the principal that is the subject of the JWT" (RFC 7519 §4.1.2) — the
/// user an on-behalf-of token speaks for.
const SUBJECT_CLAIM: &str = "sub";

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// An unsecured compact JWS carrying `claims`, which is the form a test can build
    /// without a signing key.
    fn unsecured(claims: &::serde_json::Value) -> String {
        format!(
            "{}.{}.",
            jwt::encode_base64url(br#"{"alg":"none","typ":"JWT"}"#),
            jwt::encode_base64url(claims.to_string().as_bytes())
        )
    }

    /// A compact JWS whose header names a signing algorithm and whose signature segment
    /// carries bytes — the form a caller presents as its identity provider's token. Nothing
    /// signed it, and nothing here claims otherwise.
    fn presented(claims: &::serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            jwt::encode_base64url(br#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#),
            jwt::encode_base64url(claims.to_string().as_bytes()),
            jwt::encode_base64url(b"not-a-signature")
        )
    }

    /// RULE: a token inside its own window naming a subject is accepted and answers that
    /// subject; every other presented value is refused, and only an expired one is refused
    /// as the expiry class.
    #[test]
    fn only_a_readable_token_inside_its_window_names_a_user() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let seconds = 1_800_000_000_i64;
        let cases: Vec<(&str, String, Result<String, UserTokenRefusal>)> = vec![
            (
                "inside its window",
                presented(&::serde_json::json!({
                    "sub": "user-1",
                    "iat": seconds - 60,
                    "exp": seconds + 60,
                })),
                Ok("user-1".to_string()),
            ),
            (
                "an opaque subject",
                presented(&::serde_json::json!({
                    "sub": "a3f1c0de-0000-4a1b-9c2d-5e6f70819203",
                    "exp": seconds + 1,
                })),
                Ok("a3f1c0de-0000-4a1b-9c2d-5e6f70819203".to_string()),
            ),
            (
                "presented under the bearer scheme",
                format!(
                    "Bearer {}",
                    presented(&::serde_json::json!({"sub": "user-2", "exp": seconds + 60}))
                ),
                Ok("user-2".to_string()),
            ),
            (
                "expired a moment ago",
                presented(&::serde_json::json!({"sub": "user-1", "exp": seconds - 1})),
                Err(UserTokenRefusal::Expired),
            ),
            (
                "expiring at this very instant",
                presented(&::serde_json::json!({"sub": "user-1", "exp": seconds})),
                Err(UserTokenRefusal::Expired),
            ),
            (
                "not yet valid",
                presented(&::serde_json::json!({
                    "sub": "user-1",
                    "nbf": seconds + 60,
                    "exp": seconds + 120,
                })),
                Err(UserTokenRefusal::Invalid),
            ),
            (
                "naming no expiry",
                presented(&::serde_json::json!({"sub": "user-1"})),
                Err(UserTokenRefusal::Invalid),
            ),
            (
                "naming no subject",
                presented(&::serde_json::json!({"exp": seconds + 60})),
                Err(UserTokenRefusal::Invalid),
            ),
            (
                "unsecured, so nothing signed it",
                unsecured(&::serde_json::json!({"sub": "user-1", "exp": seconds + 60})),
                Err(UserTokenRefusal::Invalid),
            ),
            (
                "three segments that are not base64url JSON",
                "abc.def.ghi".to_string(),
                Err(UserTokenRefusal::Invalid),
            ),
        ];

        for (case, token, expected) in cases {
            let read = user_of_presented_token(&token, now);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the expiry class carries AWS's own fixed sentence, and neither sentence quotes
    /// the presented token back.
    #[test]
    fn each_refusal_class_carries_its_own_fixed_sentence() {
        assert_eq!(
            UserTokenRefusal::Expired.message(),
            "Token has expired",
            "case \"expired\": expected the fixed text of the expiry class"
        );
        assert_ne!(
            UserTokenRefusal::Invalid.message(),
            UserTokenRefusal::Expired.message(),
            "case \"invalid\": expected a sentence of its own"
        );
    }
}
