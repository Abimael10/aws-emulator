// SPDX-License-Identifier: CC0-1.0

//! Why a value presented as an `AccessToken` that this service holds no issuance record
//! for is refused: whether it is a SIGNED token of one of this service's pools whose
//! signature does not check out against the key set that pool publishes, or a value that
//! is no signed token of this service at all.
//!
//! An access token a pool issues is a compact JWS signed with that pool's own key, and the
//! pool publishes the verifying half at `<iss>/.well-known/jwks.json`
//! ([`UserPool::jwks`]) — so a caller can rewrite any part of a token it holds. Rewriting
//! one does not make it a token of the pool: the signature no longer checks out over the
//! bytes presented, which is a DIFFERENT refusal from the one a value that is not a signed
//! token of this service gets, and the two are told apart here rather than by each
//! operation that takes a token.

use super::*;

/// The message a self-service operation owes an access token whose SIGNATURE does not
/// check out against the key set the pool it names publishes
/// ([`TokenRejection::SignatureNotVerified`]).
///
/// Amazon Cognito reports that it could not verify the signature of the access token it
/// was presented. The sentence names no value from the request — not the pool, the key or
/// the token — so every such refusal carries this one sentence, and every operation that
/// resolves an access token reads it here so the two cannot disagree. It is told apart
/// from the refusal of a value that is no signed token of this service at all, which
/// reports the token as invalid instead.
pub const ACCESS_TOKEN_SIGNATURE_UNVERIFIED_MESSAGE: &str =
    "Could not verify signature for Access Token";

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Why a value presented as an `AccessToken`, for which this service holds no
    /// issuance record, is refused.
    ///
    /// A pool signs the tokens it issues and publishes the key that verifies them, so a
    /// value no record resolves is still checked AGAINST that published key: one that is a
    /// compact JWS naming a pool of this service as its issuer, and whose signature does
    /// not check out over the bytes presented, is refused as a signature this service
    /// could not verify ([`TokenRejection::SignatureNotVerified`]) — which is what a
    /// token rewritten after it was signed, or signed by a key the pool does not publish,
    /// is. Anything else — a value that is not a compact JWS, an UNSECURED one (RFC 7519
    /// §6.1 names no signature to check), one naming no pool of this service, and one
    /// whose signature does check out but which this service has no record of — resolves
    /// to no access token at all ([`TokenRejection::NotAnAccessToken`]).
    ///
    /// A PURE read: the published key set is rendered from the stored pool and nothing is
    /// written, so asking changes nothing a later caller sees.
    pub(super) fn unissued_access_token_rejection(&self, presented: &str) -> TokenRejection {
        // `read` refuses the unsecured form, so a token carrying no signature never
        // reaches the check below: there is nothing to verify, and it is simply not a
        // token this service issued.
        let Ok(token) = ::service_common::jwt::read(presented) else {
            return TokenRejection::NotAnAccessToken;
        };
        let Some(pool) = token
            .claim_str(::service_common::jwt::ISSUER_CLAIM)
            .and_then(|issuer| self.pool_publishing_issuer(issuer))
        else {
            return TokenRejection::NotAnAccessToken;
        };
        match ::service_common::jwt::verify(presented, &pool.jwks()) {
            // The key the token names is one the pool publishes and did not sign these
            // bytes, or the pool publishes no such key at all: either way the signature
            // of a token naming this pool could not be verified against what the pool
            // publishes.
            Err(::service_common::jwt::SignatureDefect::Invalid
            | ::service_common::jwt::SignatureDefect::UnknownKey) => {
                TokenRejection::SignatureNotVerified
            }
            // Verified, or a question this service cannot decide (an algorithm or a key
            // family no signature is checked under here): neither says the signature
            // failed to check out, so the value is refused as the non-token it is.
            _ => TokenRejection::NotAnAccessToken,
        }
    }

    /// The pool whose published key set a token claiming `issuer` is verified against, or
    /// `None` when this service holds no pool publishing that issuer.
    ///
    /// A pool's issuer URL ends in the pool's own id ([`UserPool::issuer`]), which is how
    /// the pool a token names is found; the whole URL still has to agree, because a pool
    /// answers only for an issuer IT publishes ([`UserPool::publishes_issuer`]) — so a
    /// value naming another issuer that happens to end in a pool's id names no pool of
    /// this service.
    fn pool_publishing_issuer(&self, issuer: &str) -> Option<&UserPool> {
        let user_pool_id = issuer.trim_end_matches('/').rsplit('/').next()?;
        let pool = self.pool(user_pool_id)?;
        pool.publishes_issuer(issuer).then_some(pool)
    }
}
