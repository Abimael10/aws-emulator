// SPDX-License-Identifier: CC0-1.0

//! WHICH identity provider a JWT authorizer's `Issuer` names, for a provider THIS EMULATOR
//! itself serves.
//!
//! `JWTConfiguration$Issuer` is "the base domain of the identity provider that issues JSON Web
//! Tokens. For example, an Amazon Cognito user pool has the following format:
//! https://cognito-idp.{region}.amazonaws.com/{userPoolId}", and the gate in front of a `JWT`
//! route admits a token whose `iss` names THAT provider ([`super::jwt`]).
//!
//! A provider this emulator SERVES answers at every address this emulator is reached at, so one
//! provider has more than one name: the documented form above, which a relying party composes
//! from the Region and the pool's id; the emulator address a client dialled the pool at, which
//! is the only address a client on this machine resolves; and the one `iss` the pool mints its
//! tokens under, which is the discovery root a verifier reads the pool's key set from. Each of
//! them names ONE provider, and only the pool can say so — so the question is asked of the
//! service that OWNS user pools, in this same engine, and no second reading of an issuer URL is
//! written here.
//!
//! A provider this emulator serves nothing for is left exactly as it is recorded: there is no
//! other name of it to learn, and the gate compares the one value the authorizer holds.
//!
//! LOCK ORDER: this takes the user-pool service's state lock and nothing else. Nothing in that
//! service reads this one's state, so the order this data plane takes — its own store, then the
//! user pool's — is the only one taken. The sibling REST data plane resolves a
//! `COGNITO_USER_POOLS` authorizer's pools in that same order
//! (`service-com-amazonaws-apigateway-backplanecontrolservice/src/service/invoke/cognito_user_pools.rs`).

use ::service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as user_pools;

/// The issuer the provider `configured` names MINTS its tokens under, when this emulator serves
/// that provider; `None` for a provider nothing here serves.
///
/// A user pool's issuer ends in the pool's own id, which is how the pool that would publish it
/// is found, and the WHOLE value still has to be an issuer that pool publishes
/// (`UserPool::publishes_issuer`) — so a value that merely ends in a pool's id names that pool
/// no more than it names any other. Nothing is written: the pool is read for the issuer it
/// announces.
pub(super) fn minted_issuer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    configured: &str,
) -> Option<String> {
    let configured = configured.trim().trim_end_matches('/');
    let user_pool_id = configured.rsplit('/').next()?;
    let pools = user_pools::state::service_state(engine)?;
    let pools = pools.lock();
    let pool = pools.pool(user_pool_id)?;
    pool.publishes_issuer(configured).then(|| pool.issuer())
}
