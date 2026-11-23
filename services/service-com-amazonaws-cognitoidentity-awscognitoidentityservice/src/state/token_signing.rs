// SPDX-License-Identifier: CC0-1.0

//! The key this service SIGNS the OpenID Connect tokens it mints with, and the key set it
//! publishes for them.
//!
//! Amazon Cognito Identity vends an OpenID Connect token — `GetOpenIdToken` and
//! `GetOpenIdTokenForDeveloperIdentity` each answer one — and such a token is a SIGNED JWS:
//! its relying party is `sts:AssumeRoleWithWebIdentity`, and every consumer that verifies a
//! JWT reads `alg` and `kid` out of the header, finds that key in the issuer's published key
//! set and checks the signature over `base64url(header) '.' base64url(claims)` (RFC 7515
//! §5.2). A token whose header names the unsecured algorithm of RFC 7519 §6, or whose third
//! segment is empty, is one no issuer signed: a verifier rejects it, so the flow cannot be
//! completed against one.
//!
//! The issuer is the SERVICE ([`super::OIDC_TOKEN_ISSUER`]) rather than any one pool or
//! identity — every token this service mints names that one issuer — so the signing identity
//! is the service's own and there is exactly one of it: the key pair is generated the first
//! time a token is minted and then stands for the life of the process, and the PUBLIC half is
//! published as the key set ([`token_key_set`]) a verifier in this same process checks a
//! token against.
//!
//! What a signing key IS — the key pair, the `kid` that names it, the public JWK a key set
//! publishes and the compact serialization a signed token travels in — is the workspace's one
//! JOSE signing seam ([`::service_common::jws_signing`]), beside the reading that verifies
//! such a token, so none of it is laid out here. What this module decides is only which
//! algorithm this issuer signs under and that it holds one key.
//!
//! An in-process COMPUTATION over data: it opens no listener, spawns nothing, reads nothing
//! off the host and no private parameter leaves it.

use std::sync::OnceLock;

/// The `alg` of every token this service signs, and of the JWK it publishes for the key that
/// signed it: RSASSA-PKCS1-v1_5 using SHA-256 (RFC 7518 §3.3), named by the seam that signs
/// under it. An Amazon Cognito Identity token is signed with an RSA key whose public half the
/// issuer publishes, which is the family this names.
pub const TOKEN_SIGNING_ALGORITHM: &str = ::service_common::jws_signing::RS256;

/// This issuer's one signing key, generated when the first token is minted.
///
/// Generating a key pair is CPU-bound work measured in milliseconds, so it is done ONCE —
/// never per request — and `None` is held for a cryptography library that could not produce
/// one, in which case no token is signed and none is issued.
static SIGNING_KEY: OnceLock<Option<::service_common::jws_signing::SigningKey>> =
    OnceLock::new();

/// The key this service signs its OpenID Connect tokens with, or `None` when no key could be
/// generated at all.
fn signing_key() -> Option<&'static ::service_common::jws_signing::SigningKey> {
    SIGNING_KEY
        .get_or_init(|| {
            ::service_common::jws_signing::SigningKey::generate(TOKEN_SIGNING_ALGORITHM)
        })
        .as_ref()
}

/// The JSON Web Key Set (RFC 7517 §5) this issuer publishes for the tokens it mints: the
/// PUBLIC half of its signing key and nothing private, through the shared rendering of that
/// document ([`::service_common::jws_signing::key_set`]).
///
/// A verifier holding this document checks a token this service minted exactly as RFC 7515
/// §5.2 defines ([`::service_common::jwt::verify`]), which is what makes the signature on a
/// vended token worth anything.
#[must_use]
pub fn token_key_set() -> ::serde_json::Value {
    ::service_common::jws_signing::key_set(signing_key())
}

/// `claims` as a compact JWS signed with this issuer's key: the header names the algorithm
/// and the `kid` a verifier finds that key by in [`token_key_set`].
///
/// `None` when nothing could be signed, so a token is either SIGNED or not issued — a third
/// segment that is not a signature is a token no issuer vended, and the caller answers its
/// own server error rather than handing one out.
#[must_use]
pub fn signed_token(claims: &::serde_json::Value) -> Option<String> {
    signing_key()?.signed_compact_jws(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a token this service mints is a signed JWS — three segments, a header naming a
    /// real signing algorithm and the key it was signed with, and a signature that verifies
    /// against the key set this issuer publishes.
    #[test]
    fn a_minted_token_verifies_against_the_published_key_set() {
        // The rows differ along the claim set signed, which the signature is computed over.
        for (case, claims) in [
            ("the claims of a vend", ::serde_json::json!({"sub": "us-east-1:id-1"})),
            ("an empty claim set", ::serde_json::json!({})),
        ] {
            let token = signed_token(&claims)
                .unwrap_or_else(|| panic!("case {case:?}: nothing was signed"));
            let segments: Vec<&str> = token.split('.').collect();
            assert_eq!(
                segments.len(),
                3,
                "case {case:?}: a compact JWS has three segments, got {token:?}"
            );
            assert!(
                !segments[2].is_empty(),
                "case {case:?}: a signed JWS carries a signature, got {token:?}"
            );
            let verified = ::service_common::jwt::verify(&token, &token_key_set())
                .unwrap_or_else(|defect| panic!("case {case:?}: it does not verify: {defect}"));
            assert_eq!(
                verified.algorithm, TOKEN_SIGNING_ALGORITHM,
                "case {case:?}: expected {TOKEN_SIGNING_ALGORITHM}, got {:?}",
                verified.algorithm
            );
            assert_eq!(
                verified.claims,
                *claims.as_object().expect("a claim set is a JSON object"),
                "case {case:?}: expected the claims as signed, got {:?}",
                verified.claims
            );
        }
    }

    /// RULE: the key set publishes the PUBLIC half alone — no private RSA parameter is in it
    /// — under the `kid` the tokens it signed name.
    #[test]
    fn the_published_key_set_carries_no_private_parameter() {
        let published = token_key_set();
        let keys = published["keys"]
            .as_array()
            .unwrap_or_else(|| panic!("case \"the key set\": expected keys, got {published}"));
        assert_eq!(
            keys.len(),
            1,
            "case \"the key set\": this issuer publishes one key, got {published}"
        );
        for parameter in ["d", "p", "q", "dp", "dq", "qi"] {
            assert!(
                keys[0].get(parameter).is_none(),
                "case {parameter:?}: a published entry carries no private parameter, got \
                 {published}"
            );
        }
        let token = signed_token(&::serde_json::json!({"sub": "probe"}))
            .expect("case \"the key set\": a token is signed");
        let header = ::service_common::jwt::read(&token)
            .expect("case \"the key set\": the token reads as a compact JWS")
            .header;
        assert_eq!(
            header.get("kid"),
            keys[0].get("kid"),
            "case \"the key set\": the token names the published key, got {header:?}"
        );
    }
}
