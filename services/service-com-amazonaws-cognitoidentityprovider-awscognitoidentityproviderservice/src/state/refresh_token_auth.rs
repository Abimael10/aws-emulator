// SPDX-License-Identifier: CC0-1.0

//! Whether a presented refresh token signs its user in again, and WHY it does not:
//! the one reading every refresh surface of this service shares.
//!
//! A refresh token is "a valid refresh token that can authorize the request for new
//! tokens" (`GetTokensFromRefreshToken.RefreshToken` in this service's model), and
//! `AuthFlow=REFRESH_TOKEN_AUTH` "receive[s] new ID and access tokens when you pass a
//! `REFRESH_TOKEN` parameter with a valid refresh token as the value"
//! ((`Admin`)`InitiateAuth.AuthFlow`). Valid is read off the record this emulator wrote
//! when it ISSUED the token, and what makes it invalid is decided in ONE order here, so
//! the three surfaces that take such a token — the client-side sign-in, the
//! administrative one and the token exchange — cannot tell a caller different things
//! about the same token.
//!
//! The order is what a refusal REPORTS, and each step is about a different subject:
//!
//! 1. what the value IS: a refresh token this emulator issued, or nothing it can
//!    resolve ([`RefreshTokenRefusal::Unresolvable`]);
//! 2. WHOSE it is: the app client it was issued for is the one the request names
//!    ([`RefreshTokenRefusal::DifferentClient`]) — a token of another app client is one
//!    this service did issue, so the refusal says so rather than denying it was ever
//!    issued;
//! 3. whether its USER may sign in at all: a disabled user, one in `RESET_REQUIRED`
//!    and one that never confirmed its sign-up are refused for their own state, which
//!    no token of theirs can authorize around. This precedes the token's own state
//!    because deactivating a user also "revokes all access tokens for the user"
//!    (`AdminDisableUser` in this service's model): a caller told its token was revoked
//!    would re-authenticate, which that same state refuses, while the user's state tells
//!    it to stop;
//! 4. the TOKEN's own state: one this service revoked, one the clock has expired, one a
//!    rotation has replaced, and one whose user is gone.
//!
//! Each surface words its own refusal from the [`RefreshTokenRefusal`] it is handed and
//! raises its own modelled error, because the error SHAPES differ between them; the
//! sentences themselves are stated once, here.

use super::*;

/// Why a presented refresh token does not authorize a refresh, in the order the reading
/// below decides them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTokenRefusal {
    /// The value resolves to no refresh token this emulator issued: one it never issued,
    /// or a token of another kind presented as one.
    Unresolvable,
    /// A refresh token this emulator issued for ANOTHER app client than the one the
    /// request names.
    DifferentClient,
    /// The user the token was issued to is deactivated.
    UserDisabled,
    /// The user the token was issued to must reset its password first.
    PasswordResetRequired,
    /// The user the token was issued to never confirmed its sign-up.
    UserNotConfirmed,
    /// A refresh token this service issued for this app client and has since revoked — a
    /// sign-out of its user, or a revocation of the token itself.
    Revoked,
    /// A refresh token refresh-token rotation has already replaced.
    ReuseDetected,
    /// A refresh token this service issued for this app client that still does not
    /// authorize a refresh: one past its expiry, and one naming a user that is gone.
    Unauthorized,
}

/// The message a value this emulator cannot resolve to a refresh token at all is refused
/// with. Told apart from [`UNAUTHORIZED_REFRESH_TOKEN`] — the two differ from each other
/// ONLY in punctuation, so neither may be edited into the other — and from the refusal of
/// a token issued for another app client.
pub const INVALID_REFRESH_TOKEN: &str = "Invalid Refresh Token";

/// The message a refresh token this emulator DID issue for the app client the request
/// names, and that still does not authorize the refresh, is refused with. Its own
/// sentence, distinct from [`INVALID_REFRESH_TOKEN`], which denies the token was ever
/// issued.
pub const UNAUTHORIZED_REFRESH_TOKEN: &str = "Invalid Refresh Token.";

/// The message a refresh token issued for another app client than the request names is
/// refused with: the token is one this service issued, and what it does not authorize is
/// this app client. The sentence names neither app client.
pub const REFRESH_TOKEN_DIFFERENT_CLIENT: &str = "Refresh Token has different Client";

/// The message a refresh token this service has revoked is refused with.
pub const REFRESH_TOKEN_REVOKED: &str = "Refresh Token has been revoked";

/// The message a refresh token an earlier rotation already invalidated is refused with.
pub const REFRESH_TOKEN_REUSE: &str = "Refresh token reuse detected";

/// The message a refresh of a DEACTIVATED user is refused with.
pub const USER_DISABLED: &str = "User is disabled.";

/// The message a refresh of a user that never confirmed its sign-up is refused with.
pub const USER_NOT_CONFIRMED: &str = "User is not confirmed.";

impl RefreshTokenRefusal {
    /// The sentence this refusal carries, which every refresh surface words it with.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Unresolvable => INVALID_REFRESH_TOKEN,
            Self::DifferentClient => REFRESH_TOKEN_DIFFERENT_CLIENT,
            Self::UserDisabled => USER_DISABLED,
            Self::PasswordResetRequired => PASSWORD_RESET_REQUIRED_MESSAGE,
            Self::UserNotConfirmed => USER_NOT_CONFIRMED,
            Self::Revoked => REFRESH_TOKEN_REVOKED,
            Self::ReuseDetected => REFRESH_TOKEN_REUSE,
            Self::Unauthorized => UNAUTHORIZED_REFRESH_TOKEN,
        }
    }
}

/// The user a presented refresh token signs in again, as the stored state holds it.
#[derive(Debug, Clone)]
pub struct RefreshedUser {
    /// The pool the app client the token was issued for belongs to.
    pub user_pool_id: String,
    /// The username as STORED, which is what the refreshed tokens are minted for.
    pub username: String,
}

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Whether `refresh_token` authorizes a refresh for the app client `client_id` of
    /// `user_pool_id`, and the stored user it signs in again — or why it does not
    /// ([`RefreshTokenRefusal`], whose order this is).
    ///
    /// A PURE read: every refusal a refresh owes is decided here, before anything is
    /// written.
    pub fn resolve_refresh_token(
        &self,
        user_pool_id: &str,
        client_id: &str,
        refresh_token: &str,
        now: SystemTime,
    ) -> Result<RefreshedUser, RefreshTokenRefusal> {
        let record = self
            .token(refresh_token)
            .filter(|record| record.kind == TokenKind::Refresh)
            .ok_or(RefreshTokenRefusal::Unresolvable)?;
        if record.client_id != client_id || record.user_pool_id != user_pool_id {
            return Err(RefreshTokenRefusal::DifferentClient);
        }
        // The record is only a pointer into the pool: the user it names may have been
        // disabled, reset or deleted since the token was issued — and a user created
        // later under that name carries a different id, so it is not the user this token
        // was issued to.
        let user = self
            .pool(user_pool_id)
            .and_then(|pool| pool.user(&record.username))
            .filter(|user| user.sub == record.sub)
            .ok_or(RefreshTokenRefusal::Unauthorized)?;
        if !user.enabled {
            return Err(RefreshTokenRefusal::UserDisabled);
        }
        match user.status {
            UserStatus::ResetRequired => return Err(RefreshTokenRefusal::PasswordResetRequired),
            UserStatus::Unconfirmed => return Err(RefreshTokenRefusal::UserNotConfirmed),
            _ => {}
        }
        if record.revoked {
            return Err(RefreshTokenRefusal::Revoked);
        }
        if record.expires_at <= now {
            return Err(RefreshTokenRefusal::Unauthorized);
        }
        if record.rotated_out_by(now) {
            return Err(RefreshTokenRefusal::ReuseDetected);
        }
        Ok(RefreshedUser {
            user_pool_id: user_pool_id.to_string(),
            username: user.username.clone(),
        })
    }
}
