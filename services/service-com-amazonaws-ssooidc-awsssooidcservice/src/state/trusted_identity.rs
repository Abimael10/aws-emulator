// SPDX-License-Identifier: CC0-1.0

//! The JWT-BEARER grant of `CreateTokenWithIAM`: whether the assertion presented is one a
//! trusted token issuer signed, and which Identity Store user its claims name.
//!
//! The `assertion` is "the JSON Web Token (JWT) issued by a trusted token issuer. To
//! authorize a trusted token issuer, configure the JWT Bearer `GrantOptions` for the
//! application" (`CreateTokenWithIAMRequest$assertion`), and `clientId` "is an application
//! ARN that has OAuth grants configured". Every record that sentence names belongs to
//! another service: the application and the trusted token issuers are IAM Identity Center's
//! own, the users the claims resolve against are the identity store's, and the key set the
//! signature is checked with is the issuer's. So nothing of them is copied here — each is
//! read where it lives, in this same process, and this module is only the ORDER those facts
//! are asked in:
//!
//! 1. the value is a compact JWS at all, and is inside its own validity window
//!    (RFC 7519 §4.1.4/§4.1.5; RFC 7523 §3 requires the `exp` a window needs);
//! 2. the `iss` it names is an issuer this emulator itself serves, so the key set that
//!    issuer publishes is held in this same process ([`emulated_issuer`]);
//! 3. some trusted token issuer of this account names that issuer — a fact about the
//!    ASSERTION, settled before the client the request names, so an assertion no issuer of
//!    the account signed is refused the same way whatever client presents it;
//! 4. the signature checks out against that key set (RFC 7515 §5.2);
//! 5. the `clientId` names an application of this account, that application has the
//!    JWT-bearer grant configured with an authentication method for the IAM actor making
//!    the exchange, and that grant authorizes the issuer (and the audience) the assertion
//!    carries;
//! 6. the claim the issuer's configuration names resolves to exactly one user of the
//!    application's identity store.
//!
//! Each step is decided from what the step before it established, and every refusal is
//! reported as the CLASS it belongs to ([`AssertionRefusal`]) rather than as prose: an
//! assertion that is not one this exchange may spend is an invalid grant, a client that may
//! not make the exchange at all is an unauthorized client, and an assertion that verifies
//! but names nobody is an identity this instance does not hold — three different answers to
//! the caller.
//!
//! An in-process read: no listener, no outbound connection, nothing off the host.

use std::time::SystemTime;

use ::service_com_amazonaws_identitystore_awsidentitystore as identity_store;
use ::service_com_amazonaws_ssoadmin_swbexternalservice as identity_center;
use ::service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as user_pools;
use ::service_common::jwt;

use identity_center::ops::types::{AuthenticationMethod, Grant, TrustedTokenIssuerConfiguration};

/// The claim an identity-bound token carries its trusted context assertion in: the
/// `awsAdditionalDetails.identityContext` of a response "provides access to
/// `sts:identity_context` claim in the `idToken`"
/// (`smithy_query(query="doc",shape="com.amazonaws.ssooidc#AwsAdditionalDetails",member="identityContext")`),
/// so the two carry the same value and this is the name the token carries it under.
pub const IDENTITY_CONTEXT_CLAIM: &str = "sts:identity_context";

/// The host IAM Identity Center issues the identity token of an exchange from; the issuer
/// itself names the INSTANCE that issued it ([`identity_center_issuer`]).
pub const IDENTITY_CENTER_ISSUER_HOST: &str = "identitycenter.amazonaws.com";

/// The `iss` of an identity token this service mints for an instance: the instance is what
/// holds the application, the trusted token issuers and the identity store the token's
/// claims come from, so the token names that instance as the principal that issued it
/// (RFC 7519 §4.1.1).
pub fn identity_center_issuer(instance_arn: &str) -> String {
    let instance_id = instance_arn.rsplit('/').next().unwrap_or(instance_arn);
    format!("https://{IDENTITY_CENTER_ISSUER_HOST}/{instance_id}")
}

/// The algorithm every identity token of this service is signed under: ECDSA on P-384
/// (RFC 7518 §3.4), one of the two the workspace's signing seam generates keys for.
pub const ID_TOKEN_SIGNING_ALGORITHM: &str = ::service_common::jws_signing::ES384;

/// The claim set of the identity token issued beside an identity-bound access token: what
/// that token says about the user it identifies, and nothing else.
///
/// Every claim is read off the token being minted and the identity it is bound to — the
/// instance that issued it (`iss`), the Identity Store user it names (`sub`, RFC 7519
/// §4.1.2), the application the exchange was made for (`aud`, §4.1.3, and
/// [`APPLICATION_CLAIM`]), the window the access token beside it was minted for
/// (`iat`/`exp`, §4.1.6/§4.1.4), the directory the subject lives in
/// ([`IDENTITY_STORE_CLAIM`]) and the account that directory belongs to
/// ([`INSTANCE_ACCOUNT_CLAIM`]), the trusted token issuer that acted for the user
/// ([`ACTOR_CLAIM`]), and the trusted context assertion the response reports as
/// `awsAdditionalDetails.identityContext` ([`IDENTITY_CONTEXT_CLAIM`]).
pub fn identity_token_claims(
    token: &super::IssuedToken,
    identity: &BoundIdentity,
    identity_context: &str,
) -> ::serde_json::Value {
    let mut claims = ::serde_json::Map::new();
    claims.insert(
        jwt::ISSUER_CLAIM.to_string(),
        identity_center_issuer(&identity.instance_arn).into(),
    );
    claims.insert(SUBJECT_CLAIM.to_string(), identity.user_id.clone().into());
    claims.insert(
        jwt::AUDIENCE_CLAIM.to_string(),
        token.client_id.clone().into(),
    );
    claims.insert(
        ISSUED_AT_CLAIM.to_string(),
        super::epoch_seconds(token.issued_at).into(),
    );
    claims.insert(
        jwt::EXPIRY_CLAIM.to_string(),
        super::epoch_seconds(token.expires_at()).into(),
    );
    claims.insert(
        IDENTITY_STORE_CLAIM.to_string(),
        identity.identity_store_id.clone().into(),
    );
    claims.insert(
        INSTANCE_ACCOUNT_CLAIM.to_string(),
        ::engine::emulator_account_id().into(),
    );
    claims.insert(
        APPLICATION_CLAIM.to_string(),
        token.client_id.clone().into(),
    );
    claims.insert(IDENTITY_CONTEXT_CLAIM.to_string(), identity_context.into());
    // RFC 8693 §4.1: `act` names "the party that is authorized to act on behalf of the
    // subject", as a JSON object whose `sub` identifies that party. The party here is the
    // trusted token issuer whose assertion was exchanged for this token.
    claims.insert(
        ACTOR_CLAIM.to_string(),
        ::serde_json::json!({ SUBJECT_CLAIM: identity.trusted_token_issuer_arn }),
    );
    ::serde_json::Value::Object(claims)
}

/// The claim naming the party that ACTED for the subject of a token (RFC 8693 §4.1).
const ACTOR_CLAIM: &str = "act";

/// The claim naming the DIRECTORY the subject of an identity-bound token lives in: the
/// `IdentityStoreId` of the identity store of the instance that issued the token, which is
/// what a service receiving the propagated identity resolves the user against.
const IDENTITY_STORE_CLAIM: &str = "aws:identity_store_id";

/// The claim naming the ACCOUNT the issuing instance and its identity store belong to.
const INSTANCE_ACCOUNT_CLAIM: &str = "aws:instance_account";

/// The claim naming the APPLICATION the exchange was made for — the `clientId` of the
/// request, which "is an application ARN that has OAuth grants configured".
const APPLICATION_CLAIM: &str = "aws:application_arn";

/// The claim naming WHO a token is about — "the principal that is the subject of the JWT"
/// (RFC 7519 §4.1.2).
const SUBJECT_CLAIM: &str = "sub";

/// The claim naming when a token was issued (RFC 7519 §4.1.6).
const ISSUED_AT_CLAIM: &str = "iat";

/// Why an assertion is not one this exchange can issue a token for.
///
/// The variants are the CLASSES the operation answers, not the individual checks: several
/// checks share one answer deliberately, so an assertion tells whoever presents it nothing
/// about which record of the account it failed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssertionRefusal {
    /// The assertion is not one this exchange may spend: not a token, unsecured, outside
    /// its own window, signed by no key the issuer it names publishes, from an issuer this
    /// account trusts for nothing, or from one this application's grant does not authorize
    /// — including an assertion naming an audience the grant does not list.
    InvalidAssertion,
    /// The assertion was already spent by an earlier exchange (RFC 7523 §3: an assertion
    /// carrying a `jti` is good for one use).
    ReplayedAssertion,
    /// `clientId` names no application of this account.
    UnknownClient,
    /// The application exists and may not make this exchange: it has no JWT-bearer grant
    /// configured, carries no authentication method for the IAM actor making the exchange,
    /// or is not enabled.
    UnauthorizedClient,
    /// The assertion verified against the issuer the application authorizes, and the claim
    /// that issuer's configuration names resolves to no user of the application's identity
    /// store.
    UnresolvedIdentity,
    /// A store this exchange reads is unreachable — this service's own failure, not the
    /// request's.
    StateUnavailable,
}

/// The Identity Center identity a token is bound to: the user of the identity store the
/// assertion's claims resolved to, the instance that holds both, and the trusted token
/// issuer whose assertion asserted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundIdentity {
    /// The `InstanceArn` of the IAM Identity Center instance the application belongs to.
    pub instance_arn: String,
    /// The `IdentityStoreId` of that instance's identity store.
    pub identity_store_id: String,
    /// The `UserId` of the user the assertion's claim resolved to.
    pub user_id: String,
    /// The `TrustedTokenIssuerArn` of the issuer whose assertion was exchanged: the party
    /// that ACTED for the user, which the identity token names (see [`ACTOR_CLAIM`]).
    pub trusted_token_issuer_arn: String,
}

/// What a verified assertion entitles the exchange to.
#[derive(Clone, Debug)]
pub struct ResolvedAssertion {
    /// The identity the token this exchange mints is bound to.
    pub identity: BoundIdentity,
    /// The access scopes the application is configured with, which are the ones a request
    /// naming no `scope` is authorized for beside the documented defaults.
    pub application_scopes: Vec<String>,
    /// The `jti` the assertion carries, when it carries one: the identifier its single use
    /// is recorded under.
    pub assertion_id: Option<String>,
    /// The assertion's own `exp` as an instant, which is how long its single-use record has
    /// to be kept: past it the assertion is refused for being outside its window anyway.
    pub expires_at: SystemTime,
}

/// One trusted token issuer of this account, as this exchange reads it: the ARN a grant
/// authorizes it by, and the OIDC JWT configuration that says which claim of an assertion
/// names an identity and which attribute of the identity store that claim is matched
/// against.
#[derive(Clone, Debug)]
struct TrustedIssuer {
    arn: String,
    instance_arn: String,
    claim_attribute_path: String,
    identity_store_attribute_path: String,
}

/// The application `clientId` names, as this exchange reads it.
#[derive(Clone, Debug)]
struct AuthorizingApplication {
    instance_arn: String,
    identity_store_arn: String,
    enabled: bool,
    /// Whether the application carries an authentication method for the IAM actor: the
    /// JWT-bearer exchange is made by an IAM entity acting for the user the assertion
    /// names, and an application that configures no actor authorizes no actor.
    authenticates_an_iam_actor: bool,
    /// The `(TrustedTokenIssuerArn, AuthorizedAudiences)` pairs the application's
    /// JWT-bearer grant lists, or `None` when it has no such grant configured.
    authorized_token_issuers: Option<Vec<(String, Vec<String>)>>,
    /// The application's configured access scopes, in their stored order.
    access_scopes: Vec<String>,
}

/// The identity the assertion `assertion` names for the application `client_id`, or the
/// class it is refused as.
///
/// `region` is the Region the request was addressed to, which is what IAM Identity Center
/// resolves its instances for; `now` is the instant the exchange is made at. Takes each
/// other service's state lock in turn and holds none of them across another, and holds
/// NONE of this service's: the caller resolves the assertion before it locks its own store
/// to mint.
pub fn resolve_jwt_bearer_assertion<Eng: Send + Sync + 'static>(
    engine: &Eng,
    client_id: &str,
    assertion: &str,
    region: &str,
    now: SystemTime,
) -> Result<ResolvedAssertion, AssertionRefusal> {
    // 1. A token at all, inside its own window. An assertion carrying no `exp` has no
    //    window to be inside, and RFC 7523 §3 admits none: "The JWT MUST contain an exp
    //    (expiration time) claim".
    let token = jwt::read(jwt::without_bearer_scheme(assertion))
        .map_err(|_| AssertionRefusal::InvalidAssertion)?;
    if token.outside_window(now).is_some() {
        return Err(AssertionRefusal::InvalidAssertion);
    }
    let expires_at = expiry_instant(&token).ok_or(AssertionRefusal::InvalidAssertion)?;
    let issuer = token
        .claim_str(jwt::ISSUER_CLAIM)
        .unwrap_or_default()
        .trim()
        .to_string();
    if issuer.is_empty() {
        return Err(AssertionRefusal::InvalidAssertion);
    }

    // 2. WHICH issuer the assertion names, as this emulator knows it: the user pool that
    //    publishes that URL, and the key set it publishes for it. One pass under the user
    //    pools service's lock, which is released before anything else is read.
    let Some(claimed_issuer) = emulated_issuer(engine, &issuer) else {
        // An issuer publishes its key set at its own endpoint, outside this emulator, and
        // nothing here fetches one: the signature cannot be checked, so the assertion is
        // not accepted.
        return Err(AssertionRefusal::InvalidAssertion);
    };

    // 3. Everything IAM Identity Center holds for this exchange, read under ITS lock once
    //    and judged below, so no other service's state is read while that lock is held.
    let (trusted_issuers, application) =
        identity_center_records(engine, client_id, &claimed_issuer, region, now)?;
    if trusted_issuers.is_empty() {
        return Err(AssertionRefusal::InvalidAssertion);
    }

    // 4. The signature, against the key set that issuer publishes in this process
    //    (RFC 7515 §5.2).
    if jwt::verify(jwt::without_bearer_scheme(assertion), &claimed_issuer.key_set).is_err() {
        return Err(AssertionRefusal::InvalidAssertion);
    }

    // 5. The application, its grant, and the issuers that grant authorizes. An
    //    application may authorize SEVERAL trusted token issuers, so every entry of its
    //    grant that names this assertion's issuer is a candidate: each carries its own
    //    attribute paths, and the exchange is authorized if any of them resolves the
    //    identity.
    let application = application.ok_or(AssertionRefusal::UnknownClient)?;
    let Some(authorized) = application.authorized_token_issuers.as_ref() else {
        // RFC 6749 §5.2's `unauthorized_client`: "The authenticated client is not
        // authorized to use this authorization grant type."
        return Err(AssertionRefusal::UnauthorizedClient);
    };
    if !application.enabled || !application.authenticates_an_iam_actor {
        return Err(AssertionRefusal::UnauthorizedClient);
    }
    let candidates: Vec<&TrustedIssuer> = authorized
        .iter()
        .filter_map(|(arn, audiences)| {
            let issuer = trusted_issuers.iter().find(|trusted| {
                &trusted.arn == arn && trusted.instance_arn == application.instance_arn
            })?;
            // "AuthorizedAudiences: An array list of allowed audiences, or applications,
            // that can consume the tokens generated by the associated trusted token
            // issuer" (`AuthorizedTokenIssuer$AuthorizedAudiences`): an entry that lists
            // audiences authorizes an assertion issued for one of them, and one listing
            // none restricts no audience.
            let audience_authorized = audiences.is_empty()
                || token
                    .audiences()
                    .iter()
                    .any(|claimed| audiences.iter().any(|allowed| allowed == claimed));
            audience_authorized.then_some(issuer)
        })
        .collect();
    if candidates.is_empty() {
        // The assertion comes from an issuer this account trusts for something, and not
        // from one this application's JWT-bearer grant authorizes for this audience: it is
        // no grant this exchange may spend.
        return Err(AssertionRefusal::InvalidAssertion);
    }

    // 6. The identity the assertion's claim names, in the application's identity store.
    for authorizing_issuer in candidates {
        let Some(claimed) = claim_at(&token.claims, &authorizing_issuer.claim_attribute_path)
        else {
            continue;
        };
        let user_id = match resolve_identity_store_user(
            engine,
            &application.identity_store_arn,
            &authorizing_issuer.identity_store_attribute_path,
            &claimed,
        ) {
            Ok(user_id) => user_id,
            // This service's own failure is reported as itself; an issuer whose
            // configuration resolves nobody leaves the next candidate to try.
            Err(AssertionRefusal::StateUnavailable) => {
                return Err(AssertionRefusal::StateUnavailable)
            }
            Err(_) => continue,
        };
        return Ok(ResolvedAssertion {
            identity: BoundIdentity {
                identity_store_id: identity_store::state::identity_store_key(
                    &application.identity_store_arn,
                )
                .unwrap_or_default(),
                instance_arn: application.instance_arn,
                user_id,
                trusted_token_issuer_arn: authorizing_issuer.arn.clone(),
            },
            application_scopes: application.access_scopes,
            assertion_id: token
                .claim_str(JWT_ID_CLAIM)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            expires_at,
        });
    }
    Err(AssertionRefusal::UnresolvedIdentity)
}

/// The claim a JWT carries its own identifier in — "a unique identifier for the JWT"
/// (RFC 7519 §4.1.7), which is what RFC 7523 §3 says an authorization server tracks to
/// refuse a replayed assertion.
const JWT_ID_CLAIM: &str = "jti";

/// The assertion's `exp` as an instant.
fn expiry_instant(token: &jwt::CompactJws) -> Option<SystemTime> {
    let seconds = token.numeric_date(jwt::EXPIRY_CLAIM)?;
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(u64::try_from(seconds).ok()?))
}

/// What IAM Identity Center holds for this exchange: the trusted token issuers of the
/// account that name the assertion's issuer, and the application `client_id` names (`None`
/// when it names none).
///
/// One pass under IAM Identity Center's own state lock, which is released before anything
/// is judged, so no refusal below is decided while another service's state is unreachable.
fn identity_center_records<Eng: Send + Sync + 'static>(
    engine: &Eng,
    client_id: &str,
    issuer: &EmulatedIssuer,
    region: &str,
    now: SystemTime,
) -> Result<(Vec<TrustedIssuer>, Option<AuthorizingApplication>), AssertionRefusal> {
    let state =
        identity_center::state::service_state(engine).ok_or(AssertionRefusal::StateUnavailable)?;
    let mut state = state.lock();
    // The account's IAM Identity Center enablement is decided by the first request that
    // needs an instance — this request needs one — and the same call applies every
    // transition the clock already owes those instances, so this exchange resolves the same
    // application a read at this instant would resolve.
    state.on_request(region, now);

    let trusted_issuers = state
        .trusted_token_issuers
        .values()
        .filter_map(|record| {
            let TrustedTokenIssuerConfiguration::OidcJwtConfiguration(configuration) =
                &record.configuration;
            issuer
                .is_named_by(&configuration.issuer_url)
                .then(|| TrustedIssuer {
                    arn: record.trusted_token_issuer_arn.clone(),
                    instance_arn: record.instance_arn.clone(),
                    claim_attribute_path: configuration.claim_attribute_path.clone(),
                    identity_store_attribute_path: configuration
                        .identity_store_attribute_path
                        .clone(),
                })
        })
        .collect();

    let application = state.application(client_id).map(|record| AuthorizingApplication {
        instance_arn: record.instance_arn.clone(),
        identity_store_arn: record.identity_store_arn.clone(),
        enabled: record.status == identity_center::state::APPLICATION_STATUS_ENABLED,
        authenticates_an_iam_actor: record
            .authentication_methods
            .values()
            .any(|method| matches!(method, AuthenticationMethod::Iam(_))),
        authorized_token_issuers: record
            .grants
            .get(identity_center::state::GRANT_TYPE_JWT_BEARER)
            .and_then(|grant| match grant {
                Grant::JwtBearer(grant) => Some(
                    grant
                        .authorized_token_issuers
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|authorized| {
                            (
                                authorized.trusted_token_issuer_arn.unwrap_or_default(),
                                authorized.authorized_audiences.unwrap_or_default(),
                            )
                        })
                        .collect(),
                ),
                _ => None,
            }),
        access_scopes: record.access_scopes.keys().cloned().collect(),
    });

    Ok((trusted_issuers, application))
}

/// The OIDC issuer an assertion's `iss` names, as this emulator serves it: the user pool
/// that announces that URL, and the key set it publishes for it.
///
/// This emulator serves a user pool at TWO addresses, and the pool answers for both: the
/// documented regional URL of its issuer
/// (`https://cognito-idp.<region>.amazonaws.com/<UserPoolId>`) and the address the
/// emulator itself is reached at, which is what the tokens of a pool created through that
/// endpoint carry — `UserPool::publishes_issuer` is that one reading, and it is the pool's
/// own. So an issuer is held as the POOL it names rather than as the string it was written
/// as, and two URLs that name one pool name one issuer ([`Self::is_named_by`]).
#[derive(Clone, Debug)]
struct EmulatedIssuer {
    /// The `UserPoolId` of the pool that announces the claimed issuer.
    pool_id: String,
    /// The Region that pool lives in, which decides the regional URL it answers for.
    region: String,
    /// The key set the pool publishes, as the signature check reads it (RFC 7517 §5).
    key_set: ::serde_json::Value,
}

impl EmulatedIssuer {
    /// Whether the issuer URL a trusted token issuer is CONFIGURED with names this same
    /// issuer.
    ///
    /// Either it is the URL the assertion claims — a trailing `/` is not part of an
    /// issuer's identity, so the two are compared without one — or it is another address
    /// the same pool answers for, which is what an issuer configured with the documented
    /// regional URL is when the pool's own tokens are minted at the address this emulator
    /// was reached at.
    fn is_named_by(&self, configured: &str) -> bool {
        user_pools::state::is_pool_issuer(&self.region, &self.pool_id, configured.trim())
    }
}

/// The issuer `claimed` names, read from the service that OWNS it in this same shared
/// engine, or `None` when nothing in this process serves it.
///
/// A verifier checks a token against the key its header names in the key set the token's
/// ISSUER publishes. The issuers this emulator serves are its own Amazon Cognito user
/// pools: each publishes its key set at `<iss>/.well-known/jwks.json` and holds it in the
/// user-pool service's state in this engine — the one store that endpoint serves it from —
/// so the set is read from there rather than fetched over a network this emulator never
/// leaves. Only PUBLIC key parameters are read: no private key material crosses this seam.
///
/// The same reading STS and Verified Permissions make of a presented token
/// (`service-com-amazonaws-sts-awssecuritytokenservicev20110615`'s
/// `state::web_identity::published_key_set`,
/// `service-com-amazonaws-verifiedpermissions-verifiedpermissions`'s
/// `state::token::published_key_set`): a shared crate cannot hold it, because the
/// user-pool crate that answers it already depends on `service-common`, so each verifying
/// service reaches the owning store itself.
///
/// LOCK ORDER: this takes the user-pool service's state lock and nothing else, and holds
/// no other lock while it does.
fn emulated_issuer<Eng: Send + Sync + 'static>(
    engine: &Eng,
    claimed: &str,
) -> Option<EmulatedIssuer> {
    let pools = user_pools::state::service_state(engine)?;
    // A user pool's issuer URL ends in the pool's own id, which is how the pool that
    // publishes a key set is found; the whole URL still has to agree, because a pool
    // answers only for an issuer it publishes — one naming an endpoint this emulator
    // serves that pool at, which is the pool's own reading.
    let claimed = claimed.trim().trim_end_matches('/');
    let pool_id = claimed.rsplit('/').next()?.to_string();
    let pools = pools.lock();
    let pool = pools.pool(&pool_id)?;
    pool.publishes_issuer(claimed).then(|| EmulatedIssuer {
        pool_id,
        region: pool.region.clone(),
        key_set: pool.jwks(),
    })
}

/// The claim `path` names in `claims`, when the claim set carries it as a non-empty string.
///
/// `ClaimAttributePath` is "the path of the source attribute in the JWT from the trusted
/// token issuer" (`OidcJwtConfiguration$ClaimAttributePath`), so a path is read as the
/// dot-separated path into the claim set it is written as: `email` names a claim of the
/// token, `address.country` the member of a claim that is itself an object. A path naming
/// anything that is not a string names no identity.
fn claim_at(
    claims: &::serde_json::Map<String, ::serde_json::Value>,
    path: &str,
) -> Option<String> {
    let mut segments = path.split('.').map(str::trim).filter(|s| !s.is_empty());
    let mut value = claims.get(segments.next()?)?;
    for segment in segments {
        value = value.as_object()?.get(segment)?;
    }
    let claimed = value.as_str()?.trim();
    (!claimed.is_empty()).then(|| claimed.to_string())
}

/// The `UserId` of the one enabled user of the identity store `identity_store_arn` names
/// whose `attribute_path` attribute is `claimed`.
///
/// `IdentityStoreAttributePath` is "the path of the destination attribute in a JWT from IAM
/// Identity Center. The attribute mapped by this parameter is compared to the attribute
/// mapped by `ClaimAttributePath` when a trusted token issuer token is exchanged for an IAM
/// Identity Center token" (`OidcJwtConfiguration$IdentityStoreAttributePath`) — so the
/// comparison is made against the STORED user, and a store holding no such user resolves
/// nobody.
fn resolve_identity_store_user<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identity_store_arn: &str,
    attribute_path: &str,
    claimed: &str,
) -> Result<String, AssertionRefusal> {
    let state =
        identity_store::state::service_state(engine).ok_or(AssertionRefusal::StateUnavailable)?;
    let state = state.lock();
    let Some(store) = state.store(identity_store_arn) else {
        // A store nothing has written to holds no user, so it resolves nobody.
        return Err(AssertionRefusal::UnresolvedIdentity);
    };
    store
        .users
        .values()
        .filter(|user| {
            user.user_status
                .eq_ignore_ascii_case(identity_store::state::USER_STATUS_ENABLED)
        })
        .filter(|user| {
            user_attribute(user, attribute_path)
                .iter()
                .any(|held| held.eq_ignore_ascii_case(claimed))
        })
        // A claim identifies at most one user; the smallest id is taken so that a store
        // that somehow holds two is resolved the same way on every call rather than by the
        // order a hash map happens to walk.
        .map(|user| user.user_id.clone())
        .min()
        .ok_or(AssertionRefusal::UnresolvedIdentity)
}

/// The values a stored user carries at `attribute_path`.
///
/// The paths are the members of the modelled `User` a trusted token issuer's configuration
/// can be pointed at (`smithy_query(query="shape",shape="com.amazonaws.identitystore#User")`):
/// the unique `UserName`, the `Value` of any of its `Emails`, the `Id` of any of its
/// `ExternalIds`, and the store's own `UserId`. A multi-valued attribute answers every
/// value it holds, since the claim is matched against the attribute rather than against one
/// chosen entry of it. Compared case-insensitively because SCIM attribute names are
/// (RFC 7643 §2.1), as this emulator's identity store compares them for a list filter.
///
/// A path outside them answers nothing, so it resolves no user: nothing is matched on an
/// attribute the comparison cannot read.
fn user_attribute(user: &identity_store::state::User, attribute_path: &str) -> Vec<String> {
    let path = attribute_path.trim();
    if path.eq_ignore_ascii_case("userName") {
        return user.user_name.clone().into_iter().collect();
    }
    if path.eq_ignore_ascii_case("emails.value") {
        return user
            .emails
            .iter()
            .filter_map(|email| email.value.clone())
            .collect();
    }
    if path.eq_ignore_ascii_case("externalIds.id") {
        return user
            .external_ids
            .iter()
            .map(|external| external.id.clone())
            .collect();
    }
    if path.eq_ignore_ascii_case("id") || path.eq_ignore_ascii_case("userId") {
        return vec![user.user_id.clone()];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::claim_at;

    /// RULE: a claim attribute path is the dot-separated path into the assertion's claim
    /// set, and names an identity only where it leads to a non-empty string.
    ///
    /// The rows span what the rule generalises over: a top-level claim, a nested one, a
    /// path naming nothing, a path naming a non-string, and the empty values.
    #[test]
    fn a_claim_attribute_path_reads_the_claim_it_names() {
        let claims = ::serde_json::json!({
            "email": "reader@example.test",
            "address": {"country": "PT", "postal": {"code": "1000"}},
            "groups": ["admins"],
            "email_verified": true,
            "nickname": "   ",
        });
        let claims = claims
            .as_object()
            .expect("the test builds a claim set")
            .clone();
        // (case, the path, what it names)
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a top-level claim", "email", Some("reader@example.test")),
            ("a nested claim", "address.country", Some("PT")),
            ("a twice-nested claim", "address.postal.code", Some("1000")),
            ("a claim the token does not carry", "upn", None),
            ("a member of a claim that is not an object", "email.local", None),
            ("a claim that is a list", "groups", None),
            ("a claim that is a boolean", "email_verified", None),
            ("a claim that is all blanks", "nickname", None),
            ("an empty path", "", None),
        ];
        for (case, path, expected) in rows {
            let actual = claim_at(&claims, path);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected the path {path:?} to name {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: an assertion's `iss` names a configured trusted token issuer when the two
    /// agree but for a trailing `/`, and also when both name the same user pool this
    /// emulator serves — which answers for its documented regional URL and for the
    /// address the emulator itself is reached at.
    #[test]
    fn an_issuer_names_the_configured_trusted_token_issuer_of_the_same_pool() {
        let region = "us-west-2";
        let pool_id = "us-west-2_aBcDeFgHi";
        let issuer = super::EmulatedIssuer {
            pool_id: pool_id.to_string(),
            region: region.to_string(),
            key_set: ::serde_json::json!({"keys": []}),
        };
        // (case, the configured `IssuerUrl`, whether it names the claimed issuer)
        let rows: &[(&str, &str, bool)] = &[
            (
                "the pool's documented regional issuer URL",
                "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_aBcDeFgHi",
                true,
            ),
            (
                "the same URL with a trailing slash",
                "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_aBcDeFgHi/",
                true,
            ),
            (
                "the address this emulator is reached at",
                "https://127.0.0.1:9297/us-west-2_aBcDeFgHi",
                true,
            ),
            (
                "another pool of the same endpoint",
                "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_zZzZzZzZz",
                false,
            ),
            (
                "the same pool id under another region's endpoint",
                "https://cognito-idp.eu-west-1.amazonaws.com/us-west-2_aBcDeFgHi",
                false,
            ),
            (
                "a path below the issuer",
                "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_aBcDeFgHi/token",
                false,
            ),
            (
                "another service's endpoint",
                "https://oidc.us-west-2.amazonaws.com/us-west-2_aBcDeFgHi",
                false,
            ),
            ("no issuer at all", "", false),
        ];
        for (case, configured, expected) in rows {
            let actual = issuer.is_named_by(configured);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected is_named_by({configured:?}) == {expected}, got {actual}"
            );
        }
    }
}
