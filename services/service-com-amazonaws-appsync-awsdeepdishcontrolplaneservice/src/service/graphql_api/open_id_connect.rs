// SPDX-License-Identifier: CC0-1.0

//! The `OPENID_CONNECT` authorization mode of a GraphQL API: the decision taken about a call
//! from the TOKEN it presents, before any field of it resolves.
//!
//! "Describes an OpenID Connect (OIDC) configuration" (`OpenIDConnectConfig`) names what a
//! token has to satisfy to authorize a call on the API, and every member of it is a check:
//!
//! 1. `issuer` — "the issuer for the OIDC configuration. The issuer returned by discovery must
//!    exactly match the value of `iss` in the ID token", so a token of any other issuer
//!    authorizes nothing on this API;
//! 2. `clientId` — "the client identifier of the relying party at the OpenID identity
//!    provider. You can specify a regular expression so that AppSync can validate against
//!    multiple client identifiers at a time", judged against the client the token names (`aud`
//!    on an identity token, `client_id` on an access token). A configuration naming none
//!    filters nothing;
//! 3. `iatTTL` — "the number of milliseconds that a token is valid after it's issued to a
//!    user", measured from the token's own `iat` (RFC 7519 §4.1.6);
//! 4. `authTTL` — "the number of milliseconds that a token is valid after being
//!    authenticated", measured from the token's own `auth_time` (OpenID Connect Core 1.0 §2).
//!
//! Both TTLs default to `0`, which bounds nothing: a token is then held only to its own
//! validity window — `exp`, and `nbf` where it carries one (RFC 7519 §4.1.4/§4.1.5) — as every
//! consumer of a JWT holds one. A token past a window one of them DOES state is a token that
//! was valid and is not any more, so the call it is presented for authorizes nobody even
//! though the token itself has not expired.
//!
//! Where the issuer is one this emulator SERVES — a user pool of this same process, which
//! publishes its key set at `<issuer>/.well-known/jwks.json` — the signature is checked
//! against that published set ([`::service_common::jwt::verify`], the RFC 7515 §5.2 check),
//! reading only PUBLIC key parameters, so a claim set rewritten after signing no longer checks
//! out. For an issuer outside this process there is no key set to read and no network to fetch
//! one over, so such a token is admitted on its CLAIMS ALONE once they satisfy the
//! configuration — the same reading this plane gives every provider whose secret this service
//! does not hold ([`super::authorized`]), and the reason the signature checks below bite only
//! for an issuer this process publishes.
//!
//! A token that fails any of those authorizes nobody and the call is refused. Which SENTENCE
//! the caller is told says how far the decision got, as it does for the sibling token mode
//! ([`super::user_pool::Refusal`]): a value that is no readable token at all is reported as one
//! that could not be parsed ([`super::user_pool::UNREADABLE_TOKEN_MESSAGE`]) — nothing about it
//! can be judged until it has been read — and a token that WAS read and then not admitted,
//! whichever of the checks above it failed, is reported as the one refusal a token this mode
//! judged and refused earns ([`UNAUTHORIZED_TOKEN_MESSAGE`]), which is not the sentence a call
//! presenting no material of any mode the API set up earns. Nothing about the token itself is
//! answered back: why it was refused is logged and never reported.
//!
//! A token that passes them all is WHO the call speaks for: its claim set is what a resolver
//! reads as `$context.identity` ([`identity_of`]).
//!
//! Nothing here is written: the API's configuration and the issuer's key set are READ, and a
//! token is not consumed by being presented.

use ::std::time::{SystemTime, UNIX_EPOCH};

use ::serde_json::{Map as JsonMap, Value as Json};
use ::service_common::jwt;

use super::user_pool::Refusal;
use super::PlaneContext;
use crate::ops::types::OpenIDConnectConfig;

/// The authorization MODE this module decides for: `OPENID_CONNECT` of `AuthenticationType`.
pub const OPEN_ID_CONNECT_AUTH_TYPE: &str = "OPENID_CONNECT";

/// The scheme a SigV4-signed request presents its material under, which is the `AWS_IAM`
/// mode's material and never this one's.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// What a caller presenting a token this mode READ and did not admit is told: the refusal of a
/// token that was judged, which AppSync words for that class and which says nothing about which
/// of the checks the token failed.
///
/// A call presenting no material of any mode the API set up never reaches this mode at all and
/// earns the plane's own refusal of an unauthorized call instead
/// (`crate::service::event_api::UNAUTHORIZED_MESSAGE`), so the two are told apart by the
/// sentence they carry.
pub const UNAUTHORIZED_TOKEN_MESSAGE: &str = "Unauthorized";

/// The claim naming when the token was ISSUED (RFC 7519 §4.1.6), which `iatTTL` bounds the
/// token's usefulness from.
const ISSUED_AT_CLAIM: &str = "iat";

/// The claim naming when the end user was AUTHENTICATED (OpenID Connect Core 1.0 §2, "time
/// when the End-User authentication occurred"), which `authTTL` bounds it from.
const AUTHENTICATED_AT_CLAIM: &str = "auth_time";

/// The claim naming the principal the token speaks for (RFC 7519 §4.1.2).
const SUBJECT_CLAIM: &str = "sub";

/// The members a resolver reads this mode's `$context.identity` in, as the AppSync resolver
/// context reference names them for an OpenID Connect call: the token's whole claim set, the
/// issuer that signed it, and the subject it names.
const CLAIMS_MEMBER: &str = "claims";
const ISSUER_MEMBER: &str = "issuer";
const SUBJECT_MEMBER: &str = "sub";

/// How many milliseconds one second is: the two TTLs are stated in milliseconds and a
/// NumericDate claim in whole seconds, so one of the two is converted before they are
/// compared.
const MILLISECONDS_PER_SECOND: i64 = 1_000;

/// What this mode decided about one call.
#[derive(Debug, Clone)]
pub enum Decided {
    /// The mode does not decide this call: the API configured no OpenID Connect provider, the
    /// call presented nothing in the field this mode reads, or it presented material of
    /// another provider this API also configured.
    NotApplicable,
    /// The call authorizes nobody and reaches no field of the API.
    Refused(Refusal),
    /// The call is served, under what the token says about the caller.
    Admitted(Admitted),
}

/// What an admitted token says about the caller.
#[derive(Debug, Clone)]
pub struct Admitted {
    /// What a resolver reads as `$context.identity` for this caller.
    pub identity: Json,
}

/// One OpenID Connect provider the API accepts tokens of: the configuration's own members,
/// copied out of the record under one read of the store.
#[derive(Debug, Clone)]
struct ConfiguredIssuer {
    issuer: String,
    client_id: Option<String>,
    iat_ttl: i64,
    auth_ttl: i64,
}

impl ConfiguredIssuer {
    /// The provider one stored configuration describes.
    fn of(configured: &OpenIDConnectConfig) -> Self {
        Self {
            issuer: configured.issuer.trim().to_string(),
            client_id: configured.client_id.clone(),
            iat_ttl: configured.iat_ttl,
            auth_ttl: configured.auth_ttl,
        }
    }
}

/// A token this mode read and did not admit, with `reason` — which check it failed — kept for
/// this emulator's own log and never answered to the caller.
fn unadmitted(reason: String) -> Refusal {
    Refusal {
        message: UNAUTHORIZED_TOKEN_MESSAGE,
        reason,
    }
}

/// Decide `presented` under the `OPENID_CONNECT` mode of the API `context` serves, at `now`.
///
/// `presented` is the request's own `authorization` field, as this plane reads it for every
/// mode that decides a call by a presented value. A call carrying none is left to whichever
/// mode reads the material it did present, and a SIGNED call to the `AWS_IAM` mode
/// ([`super::caller`]).
///
/// LOCK ORDER: this service's own store is read and released, and the user-pool service's
/// store is taken after it and never at the same time — the order the sibling token mode takes
/// ([`super::user_pool::decided`]). Nothing in that service reads this one's state.
pub fn decided(context: &PlaneContext, presented: &str, now: SystemTime) -> Decided {
    let configured = configured_issuers(context);
    if configured.is_empty() {
        return Decided::NotApplicable;
    }
    let token = jwt::without_bearer_scheme(presented.trim());
    if token.is_empty() || token.starts_with(SIGV4_SCHEME) {
        return Decided::NotApplicable;
    }
    // Whether a value this mode cannot place is REFUSED here or left to another mode: an API
    // that also set up a mode reading the same field has a second reader for it, and an API
    // that did not has none, so such a value authorizes nobody.
    let unplaced = |refusal: Refusal| match defers_to_another_token_mode(context) {
        true => Decided::NotApplicable,
        false => Decided::Refused(refusal),
    };
    let read = match jwt::read(token) {
        Ok(read) => read,
        Err(defect) => {
            return unplaced(Refusal::unreadable(format!(
                "the presented value is not a token: {defect}"
            )))
        }
    };
    let Some(claimed) = read.claim_str(jwt::ISSUER_CLAIM).map(str::to_string) else {
        return unplaced(unadmitted(
            "the presented token names no issuer".to_string(),
        ));
    };
    let Some((provider, key_set)) = accepting_provider(context, &configured, &claimed) else {
        return unplaced(unadmitted(
            "the presented token's issuer is not one this API configured".to_string(),
        ));
    };
    // The token claims an issuer this API DOES accept, so nothing is left to another mode from
    // here on.
    let verified = match key_set {
        Some(key_set) => match jwt::verify(token, &key_set) {
            Ok(verified) => verified,
            Err(defect) => {
                return Decided::Refused(unadmitted(format!(
                    "the presented token is not one its issuer signed: {defect}"
                )))
            }
        },
        // An issuer outside this process publishes its key set where this emulator cannot read
        // it, so the token is judged on its claims alone.
        None => read,
    };
    if let Some(defect) = verified.outside_window(now) {
        return Decided::Refused(unadmitted(format!(
            "the presented token is outside its window: {defect}"
        )));
    }
    if let Some(unmatched) = client_unmatched(provider, &verified) {
        return Decided::Refused(unadmitted(unmatched));
    }
    if let Some(elapsed) = past_configured_ttl(provider, &verified, now) {
        return Decided::Refused(unadmitted(elapsed));
    }
    Decided::Admitted(Admitted {
        identity: identity_of(&verified),
    })
}

/// The OpenID Connect providers the API `context` serves accepts tokens of, in the order it
/// configured them: the one its own `openIDConnectConfig` names where it set the mode up as
/// its `authenticationType`, and the one each additional provider of this mode names.
fn configured_issuers(context: &PlaneContext) -> Vec<ConfiguredIssuer> {
    let store = context.state.lock();
    let Some(api) = store.graphql_apis.get(&context.api_id) else {
        return Vec::new();
    };
    if !super::configures(api, OPEN_ID_CONNECT_AUTH_TYPE) {
        return Vec::new();
    }
    let mut configured: Vec<ConfiguredIssuer> = Vec::new();
    if let Some(own) = api.open_id_connect_config.as_ref() {
        configured.push(ConfiguredIssuer::of(own));
    }
    configured.extend(
        api.additional_authentication_providers
            .iter()
            .filter(|provider| {
                provider.authentication_type.as_deref() == Some(OPEN_ID_CONNECT_AUTH_TYPE)
            })
            .filter_map(|provider| provider.open_id_connect_config.as_ref())
            .map(ConfiguredIssuer::of),
    );
    configured
        .into_iter()
        .filter(|configured| !configured.issuer.is_empty())
        .collect()
}

/// Whether the API `context` serves set up another mode that decides a call by the value its
/// `authorization` field carries — a Cognito user pool, or a Lambda authorizer.
///
/// A value this mode cannot place is that mode's input on such an API, and nobody's on an API
/// that configured neither; the plane's one reading of that question
/// ([`super::another_token_mode_configured`]), which the sibling token mode reads the same way.
fn defers_to_another_token_mode(context: &PlaneContext) -> bool {
    super::another_token_mode_configured(context, OPEN_ID_CONNECT_AUTH_TYPE)
}

/// Whether the configured issuer and the one a token claims are the SAME issuer.
///
/// "The issuer returned by discovery must exactly match the value of `iss` in the ID token",
/// so the comparison is over the whole value and never over a part of it; what is normalised
/// away first is only what names no different issuer — surrounding space, and a trailing path
/// separator, which an issuer URL carries or omits for the same provider.
fn names_the_same_issuer(configured: &str, claimed: &str) -> bool {
    !configured.trim().is_empty() && normalised_issuer(configured) == normalised_issuer(claimed)
}

/// One issuer value with what names no different issuer taken off it: surrounding space, and a
/// trailing path separator.
fn normalised_issuer(issuer: &str) -> String {
    issuer.trim().trim_end_matches('/').to_string()
}

/// Which configured provider accepts a token claiming `claimed`, and the key set its signature
/// is checked against where this process publishes one for that issuer — `None` for an issuer
/// no provider the API configured accepts.
///
/// A configuration accepts the claimed issuer when it IS that issuer
/// ([`names_the_same_issuer`]), and also when both values name one issuer THIS EMULATOR serves:
/// an issuer it hosts is published at every address this emulator is reached at (a user pool's
/// own reading, `UserPool::publishes_issuer`), so a configuration holding one of those
/// addresses and a token minted under another name the same provider, and the signature decides
/// the token either way.
fn accepting_provider<'configured>(
    context: &PlaneContext,
    configured: &'configured [ConfiguredIssuer],
    claimed: &str,
) -> Option<(&'configured ConfiguredIssuer, Option<Json>)> {
    if let Some(emulated) = emulated_issuer(context, claimed, configured) {
        if let Some(at) = emulated.accepted {
            return Some((configured.get(at)?, Some(emulated.key_set)));
        }
    }
    configured
        .iter()
        .find(|provider| names_the_same_issuer(&provider.issuer, claimed))
        .map(|provider| (provider, None))
}

/// An issuer this emulator itself serves: the key set it publishes, and which of the API's
/// configured issuers names that same provider.
struct Emulated {
    key_set: Json,
    accepted: Option<usize>,
}

/// `claimed` as an issuer this emulator serves, or `None` for one nothing in this process
/// publishes a key set for.
///
/// The issuers this emulator serves are its own Amazon Cognito user pools: a pool publishes its
/// key set at `<issuer>/.well-known/jwks.json` and holds it in the user-pool service's state in
/// this engine, so the set is read from there rather than fetched over a network this emulator
/// never leaves. A pool's issuer URL ends in the pool's own id, which is how the publishing
/// pool is found, and the WHOLE value still has to agree, because a pool answers only for an
/// issuer it publishes. Only PUBLIC key parameters are read.
fn emulated_issuer(
    context: &PlaneContext,
    claimed: &str,
    configured: &[ConfiguredIssuer],
) -> Option<Emulated> {
    let pools = ::service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice
        ::state::service_state(&*context.engine)?;
    let claimed = normalised_issuer(claimed);
    let user_pool_id = claimed.rsplit('/').next()?.to_string();
    let pools = pools.lock();
    let pool = pools.pool(&user_pool_id)?;
    if !pool.publishes_issuer(&claimed) {
        return None;
    }
    Some(Emulated {
        key_set: pool.jwks(),
        accepted: configured
            .iter()
            .position(|provider| pool.publishes_issuer(&normalised_issuer(&provider.issuer))),
    })
}

/// Why the client the token names does not satisfy the configuration's `clientId`, or `None`
/// when it does — including when the configuration names none, which filters nothing.
///
/// The plane's one reading of that check ([`super::client_unmatched`]), which the Cognito user
/// pool mode judges its own `appIdClientRegex` by: what differs between the two modes is only
/// the member the expression is stated in.
fn client_unmatched(configured: &ConfiguredIssuer, token: &jwt::CompactJws) -> Option<String> {
    super::client_unmatched(
        configured.client_id.as_deref(),
        token,
        CLIENT_ID_MEMBER,
        &configured.issuer,
    )
}

/// The member this mode's configuration states that expression in.
const CLIENT_ID_MEMBER: &str = "clientId";

/// Why the token is past a window the configuration bounds it to at `now`, or `None` while it
/// is inside every window it states.
///
/// `iatTTL` is "the number of milliseconds that a token is valid after it's issued to a user"
/// and `authTTL` "the number of milliseconds that a token is valid after being authenticated",
/// so each is a span measured from the instant one claim of the token states: `iat` for the
/// first and `auth_time` for the second. Both members default to `0`, which states no window
/// at all; a window a token carries no instant to measure from cannot be checked, and a check
/// nobody performed admits nothing, so such a token is refused rather than let through.
fn past_configured_ttl(
    configured: &ConfiguredIssuer,
    token: &jwt::CompactJws,
    now: SystemTime,
) -> Option<String> {
    let at = unix_milliseconds(now);
    for (window, claim) in [
        (configured.iat_ttl, ISSUED_AT_CLAIM),
        (configured.auth_ttl, AUTHENTICATED_AT_CLAIM),
    ] {
        if window <= 0 {
            continue;
        }
        let Some(stated) = token.numeric_date(claim) else {
            return Some(format!(
                "this API bounds how long a token it accepts stays valid, and the presented \
                 token carries no {claim} claim to measure that window from"
            ));
        };
        let elapsed = at.saturating_sub(stated.saturating_mul(MILLISECONDS_PER_SECOND));
        if elapsed >= window {
            return Some(format!(
                "the presented token's {claim} is {elapsed} ms old, past the {window} ms this \
                 API accepts it for"
            ));
        }
    }
    None
}

/// `now` as milliseconds since the epoch, which is the unit both TTLs are stated in.
fn unix_milliseconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// What a resolver reads as `$context.identity` for a call this mode admitted.
///
/// Every member comes from the TOKEN the call presented, and they are the ones the AppSync
/// resolver context reference names for an OpenID Connect call: the whole claim set, the issuer
/// that signed it and the subject it speaks for. A claim the token does not carry is left off
/// rather than invented.
fn identity_of(token: &jwt::CompactJws) -> Json {
    let mut reported = JsonMap::new();
    reported.insert(CLAIMS_MEMBER.to_string(), Json::Object(token.claims.clone()));
    if let Some(issuer) = token.claim_str(jwt::ISSUER_CLAIM) {
        reported.insert(ISSUER_MEMBER.to_string(), Json::String(issuer.to_string()));
    }
    if let Some(subject) = token.claim_str(SUBJECT_CLAIM) {
        reported.insert(SUBJECT_MEMBER.to_string(), Json::String(subject.to_string()));
    }
    Json::Object(reported)
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns. What an OIDC-authorized API SERVES is pinned end to
    //! end in `tests/all/graphql_api_open_id_connect_authorization.rs`, over tokens an issuer
    //! of this emulator really minted.

    use super::*;

    /// A token carrying `claims`, read back the way this module reads a presented one.
    fn token(claims: &Json) -> jwt::CompactJws {
        let presented = format!(
            "{}.{}.{}",
            jwt::encode_base64url(b"{\"alg\":\"RS256\"}"),
            jwt::encode_base64url(claims.to_string().as_bytes()),
            jwt::encode_base64url(b"not-checked-here")
        );
        jwt::read(&presented).unwrap_or_else(|defect| {
            panic!("the test's token must be readable, got {defect}");
        })
    }

    /// One instant as a `SystemTime`, from seconds since the epoch.
    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + ::std::time::Duration::from_secs(seconds)
    }

    /// A configuration bounding a token to `iat_ttl` / `auth_ttl` milliseconds.
    fn bounded(iat_ttl: i64, auth_ttl: i64) -> ConfiguredIssuer {
        ConfiguredIssuer {
            issuer: "https://issuer.example/realm".to_string(),
            client_id: None,
            iat_ttl,
            auth_ttl,
        }
    }

    /// RULE: a TTL the configuration states bounds how long after the instant its claim names
    /// a token stays valid; a TTL of zero bounds nothing, and a stated TTL with no claim to
    /// measure it from admits nothing.
    #[test]
    fn a_stated_ttl_bounds_the_token_from_the_instant_its_claim_names() {
        // The rows differ along which TTL is stated, how far past the claim the call is made
        // (inside, at the bound, past it), whether the claim is carried at all, and the
        // magnitude of the window — a sub-second one included, because the member is stated in
        // milliseconds while the claim is in seconds.
        let issued = 1_700_000_000_i64;
        let rows: &[(&str, ConfiguredIssuer, Json, u64, bool)] = &[
            (
                "no TTL stated at all",
                bounded(0, 0),
                ::serde_json::json!({"iat": issued, "auth_time": issued}),
                u64::try_from(issued).unwrap_or_default() + 86_400,
                false,
            ),
            (
                "an iatTTL the call is well inside",
                bounded(8_000, 0),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default() + 1,
                false,
            ),
            (
                "an iatTTL the call sits exactly at",
                bounded(8_000, 0),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default() + 8,
                true,
            ),
            (
                "an iatTTL the call is well past",
                bounded(8_000, 0),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default() + 18,
                true,
            ),
            (
                "an iatTTL shorter than one second",
                bounded(500, 0),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default() + 1,
                true,
            ),
            (
                "an iatTTL and a token carrying no iat",
                bounded(8_000, 0),
                ::serde_json::json!({"auth_time": issued}),
                u64::try_from(issued).unwrap_or_default(),
                true,
            ),
            (
                "an authTTL the call is inside",
                bounded(0, 60_000),
                ::serde_json::json!({"auth_time": issued}),
                u64::try_from(issued).unwrap_or_default() + 30,
                false,
            ),
            (
                "an authTTL the call is past",
                bounded(0, 60_000),
                ::serde_json::json!({"auth_time": issued}),
                u64::try_from(issued).unwrap_or_default() + 61,
                true,
            ),
            (
                "an authTTL and a token carrying no auth_time",
                bounded(0, 60_000),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default(),
                true,
            ),
            (
                "a long iatTTL a day-old token is past",
                bounded(3_600_000, 0),
                ::serde_json::json!({"iat": issued}),
                u64::try_from(issued).unwrap_or_default() + 86_400,
                true,
            ),
        ];
        for (case, configured, claims, asked_at, expected) in rows {
            let actual = past_configured_ttl(configured, &token(claims), at(*asked_at));
            assert_eq!(
                actual.is_some(),
                *expected,
                "case {case:?}: expected past-the-window to be {expected}, got {actual:?}"
            );
        }
    }

    /// RULE: the configured issuer and the token's `iss` have to name the SAME issuer, and
    /// nothing less than the whole value does.
    #[test]
    fn an_issuer_matches_only_the_whole_value() {
        let rows: &[(&str, &str, &str, bool)] = &[
            (
                "the same URL",
                "https://issuer.example/realm",
                "https://issuer.example/realm",
                true,
            ),
            (
                "the same URL with a trailing separator on one side",
                "https://issuer.example/realm/",
                "https://issuer.example/realm",
                true,
            ),
            (
                "the same URL with surrounding space",
                " https://issuer.example/realm ",
                "https://issuer.example/realm",
                true,
            ),
            (
                "a prefix of the configured issuer",
                "https://issuer.example/realm",
                "https://issuer.example",
                false,
            ),
            (
                "a longer path under the same host",
                "https://issuer.example/realm",
                "https://issuer.example/realm/other",
                false,
            ),
            (
                "another host entirely",
                "https://issuer.example/realm",
                "https://elsewhere.example/realm",
                false,
            ),
            ("a configuration naming no issuer", "", "", false),
        ];
        for (case, configured, claimed, expected) in rows {
            let actual = names_the_same_issuer(configured, claimed);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: `clientId` is an expression the client the token names has to satisfy, and a
    /// configuration naming none filters nothing.
    #[test]
    fn a_configured_client_id_is_judged_against_the_client_the_token_names() {
        let with_client = |client_id: Option<&str>| ConfiguredIssuer {
            issuer: "https://issuer.example/realm".to_string(),
            client_id: client_id.map(str::to_string),
            iat_ttl: 0,
            auth_ttl: 0,
        };
        // The rows differ along the FORM of the expression (absent, empty, a literal, an
        // alternation, one that is no pattern at all) and along WHICH claim the token names
        // its client in.
        let rows: &[(&str, Option<&str>, Json, bool)] = &[
            (
                "no expression at all",
                None,
                ::serde_json::json!({"aud": "client-one"}),
                false,
            ),
            (
                "an empty expression",
                Some("  "),
                ::serde_json::json!({"aud": "client-one"}),
                false,
            ),
            (
                "a literal the aud claim matches",
                Some("client-one"),
                ::serde_json::json!({"aud": "client-one"}),
                false,
            ),
            (
                "a literal the aud claim does not match",
                Some("client-one"),
                ::serde_json::json!({"aud": "client-two"}),
                true,
            ),
            (
                "an alternation one of whose branches matches",
                Some("client-one|client-two"),
                ::serde_json::json!({"aud": "client-two"}),
                false,
            ),
            (
                "an access token naming its client in client_id",
                Some("client-one"),
                ::serde_json::json!({"client_id": "client-one"}),
                false,
            ),
            (
                "a token naming several audiences, one of them matching",
                Some("client-one"),
                ::serde_json::json!({"aud": ["other", "client-one"]}),
                false,
            ),
            (
                "an expression that is not a pattern",
                Some("client-["),
                ::serde_json::json!({"aud": "client-one"}),
                true,
            ),
            (
                "a token naming no client at all",
                Some("client-one"),
                ::serde_json::json!({"sub": "somebody"}),
                true,
            ),
        ];
        for (case, client_id, claims, expected) in rows {
            let actual = client_unmatched(&with_client(*client_id), &token(claims));
            assert_eq!(
                actual.is_some(),
                *expected,
                "case {case:?}: expected unmatched to be {expected}, got {actual:?}"
            );
        }
    }

    /// RULE: the identity an admitted token reports is read off the token and carries nothing
    /// the token does not state.
    #[test]
    fn the_reported_identity_is_read_off_the_token() {
        let claims = ::serde_json::json!({
            "iss": "https://issuer.example/realm",
            "sub": "user-1",
            "aud": "client-one",
        });
        let reported = identity_of(&token(&claims));
        assert_eq!(
            reported[CLAIMS_MEMBER], claims,
            "the whole claim set is reported, got {reported}"
        );
        assert_eq!(
            reported[ISSUER_MEMBER].as_str(),
            Some("https://issuer.example/realm"),
            "the issuer is reported, got {reported}"
        );
        assert_eq!(
            reported[SUBJECT_MEMBER].as_str(),
            Some("user-1"),
            "the subject is reported, got {reported}"
        );
        let anonymous = identity_of(&token(&::serde_json::json!({})));
        assert_eq!(
            anonymous,
            ::serde_json::json!({CLAIMS_MEMBER: {}}),
            "a token claiming nothing reports nothing but its claim set, got {anonymous}"
        );
    }
}
