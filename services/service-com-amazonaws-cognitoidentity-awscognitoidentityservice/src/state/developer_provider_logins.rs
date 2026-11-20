// SPDX-License-Identifier: CC0-1.0

//! Which KIND of login one `Logins` entry of this service carries: a credential an identity
//! PROVIDER issued, or a developer user identifier this account's own back end
//! authenticated.
//!
//! A `Logins` map names its providers by name, and each name-value pair "represents a user
//! from a public provider or developer provider. If the user is from a developer provider,
//! the name-value pair will follow the syntax `"developer_provider_name":
//! "developer_user_identifier"`. The developer provider is the 'domain' by which Cognito
//! will refer to your users"
//! (`smithy_query(query="doc",shape="GetOpenIdTokenForDeveloperIdentityInput",member="Logins")`).
//! So the name alone says which of the two an entry is, and everything a provider's login
//! is decided by — the token's own claim set, the pool's trust for its issuer, the roles it
//! states — applies to the FIRST kind only: the value of the second is no token at all but
//! an identifier out of the caller's own directory.
//!
//! A name is a provider's where this service can say which provider it is: its OWN provider
//! name, under which a token it minted is handed back; one of the names AWS publishes for a
//! public provider's login ([`PUBLIC_LOGIN_PROVIDERS`]); an Amazon Cognito user pool, whose
//! name carries the pool it is ([`super::names_cognito_user_pool`]); or a provider the
//! identity pool's own configuration declares
//! ([`super::IdentityPool::names_federated_provider`]), which is where an OpenID Connect
//! issuer and a SAML provider are named. Every OTHER name is a "domain" of the caller's own
//! back end — a developer provider — whether or not it is the one the pool was created with,
//! because nothing but the pool's configuration could make it a provider's.
//!
//! Two rules follow from the classification, each owned by the operation that takes such a
//! login:
//!
//! * an identity pool has at most ONE developer provider — "You can only specify one
//!   developer provider as part of the `Logins` map, which is linked to the identity pool"
//!   (`smithy_query(query="doc",shape="GetOpenIdTokenForDeveloperIdentity")`) — so a map
//!   naming two of them asks for an identity of two back ends at once, which no pool can
//!   serve ([`MULTIPLE_DEVELOPER_PROVIDERS_MESSAGE`]);
//! * a developer-provider entry carries no token, so it buys no credential at the exchange
//!   that redeems a login into one
//!   ([`super::IdentityPool::refuse_developer_provider_login`]).

use std::collections::HashMap;

use super::{names_cognito_user_pool, IdentityPool, COGNITO_IDENTITY_LOGIN_PROVIDER};

/// The provider names AWS publishes for a login a PUBLIC provider issued: "the available
/// provider names for `Logins` are as follows: Facebook: `graph.facebook.com`; Amazon
/// Cognito user pool: `cognito-idp.<region>.amazonaws.com/<YOUR_USER_POOL_ID>`; Google:
/// `accounts.google.com`; Amazon: `www.amazon.com`; Twitter: `api.twitter.com`; Digits:
/// `www.digits.com`" (`smithy_query(query="doc",shape="GetIdInput",member="Logins")`), and
/// `appleid.apple.com`, the issuer name of the Apple sign-in an identity pool also takes a
/// public login of.
///
/// The user-pool form is not a single name but a family of them, so it is read by
/// [`names_cognito_user_pool`] rather than listed here; the rest are the fixed names above.
/// A name on this list is a public provider's whether or not the identity pool the request
/// addresses is configured with it: whether such a login is ACCEPTED is the pool's
/// configuration to decide, which is a different question from whose login it is.
pub const PUBLIC_LOGIN_PROVIDERS: [&str; 6] = [
    "accounts.google.com",
    "api.twitter.com",
    "appleid.apple.com",
    "graph.facebook.com",
    "www.amazon.com",
    "www.digits.com",
];

/// Whether `provider_name` names a PUBLIC login provider: one of the names AWS publishes
/// ([`PUBLIC_LOGIN_PROVIDERS`]), or an Amazon Cognito user pool, whose provider name carries
/// the user pool it is.
pub fn names_public_login_provider(provider_name: &str) -> bool {
    PUBLIC_LOGIN_PROVIDERS.contains(&provider_name) || names_cognito_user_pool(provider_name)
}

/// How AWS words the refusal of a `Logins` map that names more than one DEVELOPER provider.
///
/// An identity pool is linked to ONE developer provider — "you can only specify one
/// developer provider as part of the Logins map, which is linked to the identity pool"
/// (`smithy_query(query="doc",shape="GetOpenIdTokenForDeveloperIdentity")`) — so a map
/// naming two of them names two back ends' users as one account, which no pool can serve
/// whichever domain it was created with.
///
/// Fixed text with no request- or state-derived part: it states the rule and that the map
/// broke it, naming neither the providers the map carried nor the pool's own.
pub const MULTIPLE_DEVELOPER_PROVIDERS_MESSAGE: &str =
    "An identity pool only supports one developer provider. The logins provided have more \
     than one developer provider.";

impl IdentityPool {
    /// Whether a `Logins` entry keyed by `provider_name` is a DEVELOPER provider's — a
    /// "domain" of the caller's own back end, whose value is a developer user identifier
    /// rather than a token.
    ///
    /// This pool's own `DeveloperProviderName` is one by definition. Every other name is
    /// one unless this service can say which identity PROVIDER it names: its own provider
    /// name, a published public provider or an Amazon Cognito user pool
    /// ([`names_public_login_provider`]), or a provider this pool's configuration declares
    /// ([`IdentityPool::names_federated_provider`]) — the OpenID Connect issuers and SAML
    /// providers among them.
    pub fn names_a_developer_provider(&self, provider_name: &str) -> bool {
        if self.names_developer_provider(provider_name) {
            return true;
        }
        provider_name != COGNITO_IDENTITY_LOGIN_PROVIDER
            && !names_public_login_provider(provider_name)
            && !self.names_federated_provider(provider_name)
    }

    /// The DEVELOPER providers `logins` names, each once, in provider-name order — the
    /// stable order every reading of a presented map here is made in, so a map that breaks
    /// a rule in two places is answered the same way every time.
    pub fn developer_providers_named(&self, logins: &HashMap<String, String>) -> Vec<String> {
        let mut named: Vec<String> = logins
            .keys()
            .filter(|provider_name| self.names_a_developer_provider(provider_name))
            .cloned()
            .collect();
        named.sort();
        named
    }

    /// Whether `logins` names more than one DEVELOPER provider, which is the one thing a
    /// pool linked to a single developer provider cannot serve
    /// ([`MULTIPLE_DEVELOPER_PROVIDERS_MESSAGE`]).
    pub fn logins_name_several_developer_providers(
        &self,
        logins: &HashMap<String, String>,
    ) -> bool {
        self.developer_providers_named(logins).len() > 1
    }
}
