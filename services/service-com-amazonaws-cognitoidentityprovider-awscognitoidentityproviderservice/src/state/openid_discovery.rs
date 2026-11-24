// SPDX-License-Identifier: CC0-1.0

//! What a user pool publishes about itself under its token issuer: the OpenID
//! Connect discovery document, and the paths the two documents a relying party
//! bootstraps from are published at.
//!
//! A pool's `iss` claim is the discovery root ([`UserPool::issuer`]), so the
//! documents here and the routes that serve them read the same paths: a locator
//! this pool announces and the route that answers it cannot drift.

use super::*;

/// The path under a pool's issuer its JSON Web Key Set is published at: the
/// location the `iss` claim of a token leads a verifier to (Amazon Cognito
/// Developer Guide, "Verifying a JSON web token") — the one path every OIDC issuer this
/// emulator serves publishes it at ([`::service_common::openid_discovery`]).
pub const JWKS_DOCUMENT_PATH: &str = ::service_common::openid_discovery::JWKS_DOCUMENT_PATH;

/// The path under a pool's issuer its OpenID Connect discovery document is
/// published at: the well-known location RFC 8414 §3 defines, which every OIDC
/// client and relying party reads a provider's endpoints from — from the same
/// module, for the same reason.
pub const OPENID_CONFIGURATION_DOCUMENT_PATH: &str =
    ::service_common::openid_discovery::OPENID_CONFIGURATION_DOCUMENT_PATH;

/// The `response_type` values a user pool's authorization endpoint accepts.
const RESPONSE_TYPES_SUPPORTED: [&str; 2] = ["code", "token"];

/// The subject identifier type a user pool issues: one `sub` per user, the same
/// for every relying party (OpenID Connect Core §8).
const SUBJECT_TYPES_SUPPORTED: [&str; 1] = ["public"];

/// The scopes a user pool's hosted endpoints grant.
const SCOPES_SUPPORTED: [&str; 4] = ["openid", "email", "phone", "profile"];

/// How a confidential app client authenticates at the token endpoint: with its
/// client secret, in the `Authorization` header or in the request body
/// (OpenID Connect Core §9).
const TOKEN_ENDPOINT_AUTH_METHODS_SUPPORTED: [&str; 2] =
    ["client_secret_basic", "client_secret_post"];

impl UserPool {
    /// Where this pool's HOSTED authorization server is reached: the address this emulator
    /// serves its OAuth2 endpoints for this pool's domain on, or `None` for a pool that holds
    /// no domain and therefore hosts none.
    ///
    /// A pool's managed login pages and its OAuth2 endpoints are served on the domain
    /// `CreateUserPoolDomain` created — "a user pool domain hosts managed login, an
    /// authorization server and web server for authentication in your application" (that
    /// operation's own doc) — and the CUSTOM domain is the one in force where the pool holds
    /// both, as it is for the relying party its passkeys name
    /// ([`UserPool::relying_party_id`]).
    ///
    /// The ADDRESS is this emulator's own, the one the pool publishes its issuer on
    /// ([`UserPool::issuer`]), with the domain as the first path segment: a URL this service
    /// hands a caller to fetch later has to lead back to the emulator that caller reached, and
    /// the domain string resolves to one pool, which is what makes the path this pool's own.
    /// `crate::service` is where those paths are answered.
    pub fn hosted_ui_base(&self) -> Option<String> {
        let domain = self.custom_domain.as_ref().or(self.domain.as_ref())?;
        let issuer = self.issuer();
        let origin = match issuer.rsplit_once('/') {
            Some((origin, _)) => origin.to_string(),
            None => issuer,
        };
        Some(format!("{origin}/{}", domain.domain))
    }

    /// This pool's OpenID Connect discovery document, as served at
    /// `<issuer>/.well-known/openid-configuration`.
    ///
    /// The `issuer` it names and the key set it points at are the pool's own
    /// ([`UserPool::issuer`]), the issuer this pool's tokens announce: a relying
    /// party that bootstraps from this document therefore checks the `iss` claim
    /// of a token against the issuer the pool itself states, and the key set it
    /// finds is the one that verifies the tokens that issuer mints. The signing
    /// algorithm it advertises is read from the token signer itself, so the
    /// document cannot announce an algorithm the pool does not sign with.
    ///
    /// The four endpoints a CLIENT drives — the authorization, token, user-info and
    /// revocation endpoints — are served by the pool's hosted authorization server, so the
    /// document names them under the domain that server is reached on
    /// ([`UserPool::hosted_ui_base`]), which is where this emulator answers them. A pool that
    /// holds no domain hosts no such server, and names them under its own issuer as the
    /// locators it has.
    ///
    /// A pure read: it renders stored state and changes nothing.
    pub fn openid_configuration(&self) -> serde_json::Value {
        let issuer = self.issuer();
        let hosted = self.hosted_ui_base();
        let endpoint = |hosted_path: &str, issuer_path: &str| match hosted.as_deref() {
            Some(base) => format!("{base}{hosted_path}"),
            None => format!("{issuer}{issuer_path}"),
        };
        serde_json::json!({
            "authorization_endpoint": endpoint("/oauth2/authorize", "/authorize"),
            "end_session_endpoint": endpoint("/logout", "/logout"),
            "id_token_signing_alg_values_supported": [
                super::tokens::TOKEN_SIGNING_ALGORITHM,
            ],
            "issuer": issuer,
            "jwks_uri": self.jwks_uri(),
            "response_types_supported": RESPONSE_TYPES_SUPPORTED,
            "revocation_endpoint": endpoint("/oauth2/revoke", "/revoke"),
            "scopes_supported": SCOPES_SUPPORTED,
            "subject_types_supported": SUBJECT_TYPES_SUPPORTED,
            "token_endpoint": endpoint("/oauth2/token", "/token"),
            "token_endpoint_auth_methods_supported": TOKEN_ENDPOINT_AUTH_METHODS_SUPPORTED,
            "userinfo_endpoint": endpoint("/oauth2/userInfo", "/userInfo"),
        })
    }
}

/// The sentence a request for a well-known document of a pool this emulator does
/// not hold is refused with: the user-pool endpoint reports that the pool the
/// request path named does not exist, and names that pool id.
#[must_use]
pub fn user_pool_not_found_message(user_pool_id: &str) -> String {
    format!("User pool {user_pool_id} does not exist.")
}
