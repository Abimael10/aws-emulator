// SPDX-License-Identifier: CC0-1.0

//! The `AMAZON_COGNITO_USER_POOLS` authorization mode of a GraphQL API: the decision taken
//! about a call from the TOKEN it presents, before any field of it resolves.
//!
//! "Describes an Amazon Cognito user pool configuration" (`UserPoolConfig`) names the pool a
//! call's token has to come from — `userPoolId` in `awsRegion` — and `appIdClientRegex` is "a
//! regular expression for validating the incoming Amazon Cognito user pool app client ID. If
//! this value isn't set, no filtering is applied". An API sets the mode up either as its
//! `authenticationType`, with that configuration, or as one of its
//! `additionalAuthenticationProviders`, with a `CognitoUserPoolConfig` carrying the same two
//! members ([`super::configures`] is the one reading of which modes an API set up).
//!
//! Every user pool this emulator serves is held in this same process and publishes its own
//! key set (the document a pool serves at `<issuer>/.well-known/jwks.json`), so the decision
//! is a FUNCTION OF THE TOKEN AND OF THE POOLS the API configured, computed the way any
//! consumer of a pool token computes it:
//!
//! 1. the presented value is a compact JWS at all ([`::service_common::jwt::read`], the
//!    tree's one bounded reading of one, which also refuses the unsecured form nothing
//!    signed);
//! 2. its `iss` names a pool the API configured — a token minted by any other issuer, by a
//!    pool of another account, or by a pool this emulator does not hold, is not a token this
//!    API accepts;
//! 3. the SIGNATURE checks out against that pool's published key set
//!    ([`::service_common::jwt::verify`], the RFC 7515 §5.2 check): only PUBLIC key
//!    parameters are read, so a claim set rewritten after signing no longer checks out;
//! 4. the token is inside its own validity window — `exp`, and `nbf` where it carries one
//!    (RFC 7519 §4.1.4/§4.1.5);
//! 5. where the configuration declares an `appIdClientRegex`, the app client the token names
//!    matches it — `aud` on an identity token and `client_id` on an access token, which is
//!    the shared audience reading ([`::service_common::jwt::CompactJws::audiences`]).
//!
//! A token that fails any of those authorizes nobody, and the call is refused ([`Refusal`]
//! states which sentence the caller is told: a value that is no readable token at all is
//! reported as one that could not be parsed, and a token read and not admitted earns the one
//! refusal every unauthorized call on this data plane earns). A token that passes them all is
//! WHO the call speaks for: its claim set is what a resolver reads as `$context.identity`
//! ([`Admitted::identity`]), and the groups it claims are what the group guards a schema
//! writes on its fields are judged against ([`refused_fields`]) — on a QUERY or MUTATION
//! field at the API's GraphQL endpoint, and on the SUBSCRIPTION field a `start` names at its
//! real-time one ([`super::realtime`]).
//!
//! Nothing here is written: the pool's state is READ for the key set it publishes, and a
//! token is not consumed by being presented.

use ::std::time::SystemTime;

use ::serde_json::{Map as JsonMap, Value as Json};
use ::service_common::jwt;

use super::PlaneContext;
use crate::state::DirectiveUse;

/// The authorization MODE this module decides for: `AMAZON_COGNITO_USER_POOLS` of
/// `AuthenticationType`.
pub const USER_POOL_AUTH_TYPE: &str = "AMAZON_COGNITO_USER_POOLS";

/// The two modes beside this one that decide a call by a value presented in the request's own
/// `authorization` field. An API that configured one of them is an API where a value this
/// mode cannot place is left to that mode instead of refused here.
const OPEN_ID_CONNECT_AUTH_TYPE: &str = "OPENID_CONNECT";

/// The scheme a SigV4-signed request presents its material under, which is the `AWS_IAM`
/// mode's material and never this one's.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The claims a Cognito user pool token carries that this mode reports about the caller: the
/// groups the user belongs to ("a user who is in a group … populates a `cognito:groups` claim
/// to their access and identity tokens"), and the user's own name, which an IDENTITY token
/// writes under the pool's namespaced claim and an ACCESS token under the plain one.
const GROUPS_CLAIM: &str = "cognito:groups";
const ID_TOKEN_USERNAME_CLAIM: &str = "cognito:username";
const ACCESS_TOKEN_USERNAME_CLAIM: &str = "username";

/// The claim naming the principal the token speaks for (RFC 7519 §4.1.2).
const SUBJECT_CLAIM: &str = "sub";

/// The directives a schema writes a field's group guard with: `@aws_auth(cognito_groups: …)`,
/// and `@aws_cognito_user_pools(cognito_groups: …)` for the same guard on an API that
/// configured more than one authorization mode. Both name the groups a caller has to be in to
/// reach what they are written on, under the one argument.
const GROUP_GUARD_DIRECTIVES: [&str; 2] = ["aws_auth", "aws_cognito_user_pools"];
const GROUP_GUARD_ARGUMENT: &str = "cognito_groups";

/// The `defaultAction` an API that set the mode up as an ADDITIONAL provider applies, which
/// is the value a `CognitoUserPoolConfig` carries no member for: the mode's own default, and
/// what `$context.identity.defaultAuthStrategy` then reports.
const DEFAULT_ACTION: &str = "ALLOW";

/// What this mode decided about one call.
#[derive(Debug, Clone)]
pub enum Decided {
    /// The mode does not decide this call: the API configured no user pool, the call
    /// presented nothing in the field this mode reads, or it presented material of another
    /// provider this API also configured.
    NotApplicable,
    /// The call authorizes nobody and reaches no field of the API ([`Refusal`]).
    Refused(Refusal),
    /// The call is served, under what the token says about the caller.
    Admitted(Admitted),
}

/// A refused call: what its caller is TOLD, and which check the token failed.
///
/// The two are deliberately separate. The reason is LOGGED and never answered, because
/// nothing about the token a caller presented is reported back to it; what the caller is told
/// is one of exactly two sentences, and which one says how far the decision got: a value this
/// mode could not READ as a JSON Web Token at all is reported as a token that could not be
/// parsed ([`UNREADABLE_TOKEN_MESSAGE`]) — nothing about it can be judged until it has been —
/// and a token that WAS read and then not admitted earns the one refusal every unauthorized
/// call on this data plane earns, whichever of the later checks it failed.
#[derive(Debug, Clone)]
pub struct Refusal {
    /// The sentence the refusal carries.
    pub message: &'static str,
    /// Which check the token failed, for this emulator's own log.
    pub reason: String,
}

impl Refusal {
    /// A value this mode could not read as a JSON Web Token.
    fn unreadable(reason: String) -> Self {
        Self {
            message: UNREADABLE_TOKEN_MESSAGE,
            reason,
        }
    }

    /// A token that was read and not admitted.
    fn unauthorized(reason: String) -> Self {
        Self {
            message: crate::service::event_api::UNAUTHORIZED_MESSAGE,
            reason,
        }
    }
}

/// What a caller presenting a value this mode cannot READ as a JSON Web Token is told: the
/// token has to be parsed before anything about it — its issuer, its signature, its window —
/// can be judged, so the refusal states exactly that and nothing about the value itself.
pub const UNREADABLE_TOKEN_MESSAGE: &str = "Unable to parse JWT token.";

/// What an admitted token says about the caller.
#[derive(Debug, Clone)]
pub struct Admitted {
    /// What a resolver reads as `$context.identity` for this caller.
    pub identity: Json,
    /// The groups the token claims, which the schema's group guards are judged against.
    pub groups: Vec<String>,
}

/// One user pool the API accepts tokens of: the pool, and the app-client expression the
/// configuration validates a token's audience against.
#[derive(Debug, Clone)]
struct ConfiguredPool {
    user_pool_id: String,
    app_id_client_regex: Option<String>,
    /// The action the API takes for a call this configuration does not match, which is what
    /// `$context.identity.defaultAuthStrategy` reports.
    default_action: String,
}

/// Decide `presented` under the `AMAZON_COGNITO_USER_POOLS` mode of the API `context` serves,
/// at `now`, for a call that came from `source_ip`.
///
/// `presented` is the request's own `authorization` field, as this plane reads it for every
/// mode that decides a call by a presented value. A call carrying none is left to whichever
/// mode reads the material it did present, and a SIGNED call to the `AWS_IAM` mode
/// ([`super::caller`]).
///
/// LOCK ORDER: this service's own store is read and released, and the user-pool service's
/// store is taken after it and never at the same time. Nothing in that service reads this
/// one's state, so the order taken here is the only one taken.
pub fn decided(
    context: &PlaneContext,
    presented: &str,
    source_ip: Option<String>,
    now: SystemTime,
) -> Decided {
    let configured = configured_pools(context);
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
    let Some(issuer) = read.claim_str(jwt::ISSUER_CLAIM) else {
        return unplaced(Refusal::unauthorized(
            "the presented token names no issuer".to_string(),
        ));
    };
    let Some((pool, key_set)) = issuing_pool(context, &configured, issuer) else {
        return unplaced(Refusal::unauthorized(
            "the presented token's issuer is no user pool this API configured".to_string(),
        ));
    };
    // The signature over the token's OWN encoded bytes, against the key its header names in
    // the set the pool publishes: a token minted by one pool does not verify against
    // another's key set, and a claim set rewritten after signing no longer checks out. The
    // token claims a pool this API DOES accept, so nothing is left to another mode from here
    // on.
    let verified = match jwt::verify(token, &key_set) {
        Ok(verified) => verified,
        Err(defect) => {
            return Decided::Refused(Refusal::unauthorized(format!(
                "the presented token is not one its user pool signed: {defect}"
            )))
        }
    };
    if let Some(defect) = verified.outside_window(now) {
        return Decided::Refused(Refusal::unauthorized(format!(
            "the presented token is outside its window: {defect}"
        )));
    }
    if let Some(unmatched) = audience_unmatched(&pool, &verified) {
        return Decided::Refused(Refusal::unauthorized(unmatched));
    }
    let groups = claimed_groups(&verified);
    Decided::Admitted(Admitted {
        identity: identity_of(&verified, &pool.default_action, &groups, source_ip),
        groups,
    })
}

/// The fields of `selected` a caller claiming `groups` may not access: the ones the API's
/// schema writes a GROUP GUARD on that none of those groups satisfies.
///
/// `@aws_auth(cognito_groups: ["…"])` — and `@aws_cognito_user_pools(cognito_groups: ["…"])`,
/// which is the same guard on an API configured for more than one mode — names the groups a
/// caller has to be in to reach what it is written on. A guard on the FIELD decides that
/// field; a guard on the TYPE decides every field of it that writes none of its own; and a
/// field under no guard at all is reachable by any caller this mode admitted.
///
/// `selected` is `(typeName, fieldName)` pairs. It is asked field by field as the execution
/// resolves each one ([`super::execution`]), so a guard on a field of ANY type is judged, not
/// only one on the operation's root type, and `start` on the real-time endpoint asks it for
/// the one subscription field it names ([`super::realtime`]).
pub fn refused_fields(
    context: &PlaneContext,
    selected: &[(String, String)],
    groups: &[String],
) -> Vec<(String, String)> {
    let store = context.state.lock();
    let Some(api) = store.graphql_apis.get(&context.api_id) else {
        return Vec::new();
    };
    selected
        .iter()
        .filter(|(type_name, field_name)| {
            // WHERE a directive about the field is written is the plane's one reading of that
            // question ([`super::field_auth::written_directives`]), which the authorization
            // modes a field is reserved for are judged over too; what differs here is only
            // the predicate, the GROUPS a guard names.
            let Some((on_field, on_type)) =
                super::field_auth::written_directives(api, type_name, field_name)
            else {
                return false;
            };
            let required = match required_groups(on_field) {
                Some(required) => required,
                None => match required_groups(on_type) {
                    Some(required) => required,
                    None => return false,
                },
            };
            !required.iter().any(|group| groups.contains(group))
        })
        .cloned()
        .collect()
}

/// The groups a group guard among `directives` requires, or `None` where they write none —
/// including a guard whose argument names no group at all, which states no requirement this
/// service can judge a caller against.
///
/// The names are the parser's own string values of the argument
/// (`crate::state::DirectiveArgument::strings`), so GraphQL's escaping is applied by the
/// maintained parser the schema was read with — the same reading an `@aws_subscribe` link is
/// judged on.
fn required_groups(directives: &[DirectiveUse]) -> Option<Vec<String>> {
    let named: Vec<String> = directives
        .iter()
        .filter(|directive| GROUP_GUARD_DIRECTIVES.contains(&directive.name.as_str()))
        .flat_map(|directive| {
            directive
                .arguments
                .iter()
                .filter(|argument| argument.name == GROUP_GUARD_ARGUMENT)
                .flat_map(|argument| argument.strings.clone())
        })
        .collect();
    (!named.is_empty()).then_some(named)
}

/// The user pools the API `context` serves accepts tokens of, in the order it configured
/// them: the pool its own `userPoolConfig` names where it set the mode up as its
/// `authenticationType`, and the pool each additional provider of this mode names.
fn configured_pools(context: &PlaneContext) -> Vec<ConfiguredPool> {
    let store = context.state.lock();
    let Some(api) = store.graphql_apis.get(&context.api_id) else {
        return Vec::new();
    };
    if !super::configures(api, USER_POOL_AUTH_TYPE) {
        return Vec::new();
    }
    let mut configured: Vec<ConfiguredPool> = Vec::new();
    if let Some(own) = api.user_pool_config.as_ref() {
        configured.push(ConfiguredPool {
            user_pool_id: own.user_pool_id.clone(),
            app_id_client_regex: own.app_id_client_regex.clone(),
            default_action: match own.default_action.is_empty() {
                true => DEFAULT_ACTION.to_string(),
                false => own.default_action.clone(),
            },
        });
    }
    configured.extend(
        api.additional_authentication_providers
            .iter()
            .filter(|provider| provider.authentication_type.as_deref() == Some(USER_POOL_AUTH_TYPE))
            .filter_map(|provider| provider.user_pool_config.as_ref())
            .map(|added| ConfiguredPool {
                user_pool_id: added.user_pool_id.clone(),
                app_id_client_regex: added.app_id_client_regex.clone(),
                default_action: DEFAULT_ACTION.to_string(),
            }),
    );
    configured
        .into_iter()
        .filter(|pool| !pool.user_pool_id.is_empty())
        .collect()
}

/// Whether the API `context` serves set up another mode that decides a call by the value its
/// `authorization` field carries — an OpenID Connect issuer, or a Lambda authorizer.
///
/// A value this mode cannot place is that mode's input on such an API, and nobody's on an API
/// that configured neither.
fn defers_to_another_token_mode(context: &PlaneContext) -> bool {
    let store = context.state.lock();
    store
        .graphql_apis
        .get(&context.api_id)
        .is_some_and(|api| {
            super::configures(api, OPEN_ID_CONNECT_AUTH_TYPE)
                || super::configures(api, super::authorizer::LAMBDA_AUTH_TYPE)
        })
}

/// The configured pool `issuer` names and the key set it publishes, or `None` when no pool
/// the API configured issues tokens under that issuer.
///
/// The pools the API CONFIGURED are what is looked up — not the pool the token claims — so a
/// token naming a pool this emulator holds but the API does not configure resolves no key set.
/// What is read out of the pool is the PUBLIC key set it publishes, so a token that verifies
/// here is one any holder of the same key set could have verified.
fn issuing_pool(
    context: &PlaneContext,
    configured: &[ConfiguredPool],
    issuer: &str,
) -> Option<(ConfiguredPool, Json)> {
    let pools = ::service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice
        ::state::service_state(&*context.engine)?;
    let claimed = issuer.trim().trim_end_matches('/');
    let pools = pools.lock();
    configured.iter().find_map(|configured| {
        let pool = pools.pool(&configured.user_pool_id)?;
        pool.publishes_issuer(claimed)
            .then(|| (configured.clone(), pool.jwks()))
    })
}

/// Why the app client the token names does not satisfy the configuration's
/// `appIdClientRegex`, or `None` when it does — including when the configuration declares
/// none, which is where "no filtering is applied".
///
/// The audiences read are the shared ones: an identity token names its app client in `aud`,
/// an access token of the same pool in `client_id`, and a match under either satisfies the
/// expression. The expression comes from a stored configuration rather than from this crate,
/// so it is compiled under the explicit program budget this crate compiles every
/// caller-supplied pattern under — and one past that budget REFUSES every token, because a
/// filter this emulator could not evaluate has not admitted anything: treating it as "no
/// filtering" would be a success reported for a check nobody performed.
fn audience_unmatched(configured: &ConfiguredPool, token: &jwt::CompactJws) -> Option<String> {
    let expression = configured
        .app_id_client_regex
        .as_deref()
        .map(str::trim)
        .filter(|expression| !expression.is_empty())?;
    let compiled = ::regex::RegexBuilder::new(expression)
        .size_limit(super::mapping_template::utilities::MAX_PATTERN_PROGRAM_BYTES)
        .dfa_size_limit(super::mapping_template::utilities::MAX_PATTERN_DFA_BYTES)
        .build();
    let pattern = match compiled {
        Ok(pattern) => pattern,
        Err(_) => {
            ::tracing::warn!(
                user_pool_id = %configured.user_pool_id,
                "this API's appIdClientRegex is not a regular expression this emulator \
                 evaluates within its compiled-size budget, so no token satisfies it"
            );
            return Some(
                "this API's appIdClientRegex is not an expression this emulator evaluates, \
                 so the presented token's app client could not be validated against it"
                    .to_string(),
            );
        }
    };
    if token
        .audiences()
        .iter()
        .any(|audience| pattern.is_match(audience))
    {
        return None;
    }
    Some(
        "the presented token's app client does not match this API's appIdClientRegex"
            .to_string(),
    )
}

/// The groups `token` claims, in the order it claims them.
fn claimed_groups(token: &jwt::CompactJws) -> Vec<String> {
    token
        .claims
        .get(GROUPS_CLAIM)
        .and_then(Json::as_array)
        .map(|claimed| {
            claimed
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What a resolver reads as `$context.identity` for a call this mode admitted.
///
/// Every member comes from the TOKEN the call presented: the whole claim set under `claims`,
/// the subject and the user's own name, the issuer that signed it, and the groups it claims —
/// `null` for a user in none, because a token of such a user carries no group claim at all.
/// `defaultAuthStrategy` is the action the API's own configuration takes for a call it does
/// not match, and `sourceIp` the address the call came from, left off where the request states
/// none rather than invented.
fn identity_of(
    token: &jwt::CompactJws,
    default_action: &str,
    groups: &[String],
    source_ip: Option<String>,
) -> Json {
    let mut reported = JsonMap::new();
    reported.insert(
        "claims".to_string(),
        Json::Object(token.claims.clone()),
    );
    reported.insert(
        "defaultAuthStrategy".to_string(),
        Json::String(default_action.to_string()),
    );
    reported.insert(
        "groups".to_string(),
        match groups.is_empty() {
            true => Json::Null,
            false => Json::Array(
                groups
                    .iter()
                    .map(|group| Json::String(group.clone()))
                    .collect(),
            ),
        },
    );
    if let Some(issuer) = token.claim_str(jwt::ISSUER_CLAIM) {
        reported.insert("issuer".to_string(), Json::String(issuer.to_string()));
    }
    if let Some(source_ip) = source_ip {
        reported.insert(
            "sourceIp".to_string(),
            Json::Array(vec![Json::String(source_ip)]),
        );
    }
    if let Some(subject) = token.claim_str(SUBJECT_CLAIM) {
        reported.insert("sub".to_string(), Json::String(subject.to_string()));
    }
    if let Some(username) = token
        .claim_str(ID_TOKEN_USERNAME_CLAIM)
        .or_else(|| token.claim_str(ACCESS_TOKEN_USERNAME_CLAIM))
    {
        reported.insert("username".to_string(), Json::String(username.to_string()));
    }
    Json::Object(reported)
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns. What a Cognito-authorized API SERVES is pinned end to
    //! end in `tests/all/graphql_api_user_pool_authorization.rs`, over tokens a user pool of
    //! this emulator really minted.

    use super::*;

    /// The directives one FIELD of a parsed type definition carries, read through the crate's
    /// own parse of a caller-supplied definition (`crate::state::parse_type_definition`), so a
    /// row is judged on what the maintained GraphQL parser made of the schema text rather than
    /// on a hand-built record.
    fn field_directives(definition: &str) -> Vec<DirectiveUse> {
        let parsed = crate::state::parse_type_definition(definition)
            .unwrap_or_else(|_| panic!("the test's definition must parse: {definition}"));
        parsed
            .fields
            .first()
            .map(|field| field.directives.clone())
            .unwrap_or_default()
    }

    /// RULE: both spellings of the group guard name the groups a caller has to be in, and
    /// anything else names no requirement.
    #[test]
    fn a_group_guard_names_the_groups_it_requires() {
        // The rows differ along the directive written, the argument it carries and the FORM of
        // the list: one group, several, an empty list, a name carrying a comma, a name
        // carrying an escape, and a directive that is no guard at all.
        let rows: &[(&str, &str, Option<Vec<&str>>)] = &[
            (
                "one group in a list",
                "type Query { a: String @aws_auth(cognito_groups: [\"Admins\"]) }",
                Some(vec!["Admins"]),
            ),
            (
                "two groups in a list",
                "type Query { a: String @aws_auth(cognito_groups: [\"Admins\", \"Readers\"]) }",
                Some(vec!["Admins", "Readers"]),
            ),
            (
                "the multi-mode spelling of the same guard",
                "type Query { a: String @aws_cognito_user_pools(cognito_groups: [\"Writers\"]) }",
                Some(vec!["Writers"]),
            ),
            (
                "a group name carrying a comma",
                "type Query { a: String @aws_auth(cognito_groups: [\"a,b\"]) }",
                Some(vec!["a,b"]),
            ),
            (
                "a group name carrying an escaped quotation mark",
                "type Query { a: String @aws_auth(cognito_groups: [\"a\\\"b\"]) }",
                Some(vec!["a\"b"]),
            ),
            (
                "an empty list, which states no requirement",
                "type Query { a: String @aws_auth(cognito_groups: []) }",
                None,
            ),
            (
                "the guard with no group argument",
                "type Query { a: String @aws_cognito_user_pools }",
                None,
            ),
            (
                "another directive entirely",
                "type Query { a: String @deprecated(reason: \"old\") }",
                None,
            ),
        ];
        for (case, definition, expected) in rows {
            let actual = required_groups(&field_directives(definition));
            let expected: Option<Vec<String>> = expected
                .as_ref()
                .map(|groups| groups.iter().map(|group| (*group).to_string()).collect());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the configuration's `appIdClientRegex` is matched against the app client the
    /// token names — `aud` on an identity token, `client_id` on an access token — a
    /// configuration declaring none applies no filtering, and an expression this emulator
    /// cannot evaluate admits no token.
    #[test]
    fn the_app_client_expression_is_matched_against_the_token() {
        let configured = |expression: Option<&str>| ConfiguredPool {
            user_pool_id: "us-west-2_pool".to_string(),
            app_id_client_regex: expression.map(str::to_string),
            default_action: DEFAULT_ACTION.to_string(),
        };
        let token = |claims: Json| {
            let encoded = format!(
                "{}.{}.c2lnbmF0dXJl",
                jwt::encode_base64url(b"{\"alg\":\"RS256\",\"kid\":\"k\"}"),
                jwt::encode_base64url(claims.to_string().as_bytes())
            );
            jwt::read(&encoded).expect("the test builds a well-formed compact JWS")
        };
        // The rows differ along the expression declared, which audience spelling the token
        // carries, and whether the expression is one this engine builds at all.
        let rows: &[(&str, Option<&str>, Json, bool)] = &[
            (
                "no expression at all, which filters nothing",
                None,
                ::serde_json::json!({"aud": "anything"}),
                true,
            ),
            (
                "an expression the identity token's aud matches",
                Some("^client-1$"),
                ::serde_json::json!({"aud": "client-1"}),
                true,
            ),
            (
                "an expression another app client does not match",
                Some("^client-1$"),
                ::serde_json::json!({"aud": "client-2"}),
                false,
            ),
            (
                "an access token, matched on client_id",
                Some("^client-1$"),
                ::serde_json::json!({"client_id": "client-1"}),
                true,
            ),
            (
                "a token carrying no app client at all",
                Some("^client-1$"),
                ::serde_json::json!({"sub": "u"}),
                false,
            ),
            (
                "an expression that is not a regular expression",
                Some("client-["),
                ::serde_json::json!({"aud": "client-1"}),
                false,
            ),
        ];
        for (case, expression, claims, admitted) in rows {
            let unmatched = audience_unmatched(&configured(*expression), &token(claims.clone()));
            assert_eq!(
                unmatched.is_none(),
                *admitted,
                "case {case:?}: expected admitted={admitted}, got {unmatched:?}"
            );
        }
    }

    /// RULE: the identity a resolver reads is built from the token's own claims, and reports
    /// no group for a user in none.
    #[test]
    fn the_identity_reported_is_what_the_token_claims() {
        let token = |claims: Json| {
            let encoded = format!(
                "{}.{}.c2lnbmF0dXJl",
                jwt::encode_base64url(b"{\"alg\":\"RS256\",\"kid\":\"k\"}"),
                jwt::encode_base64url(claims.to_string().as_bytes())
            );
            jwt::read(&encoded).expect("the test builds a well-formed compact JWS")
        };
        // The rows differ along the token KIND (an identity token, an access token) and along
        // whether the user belongs to any group.
        /// One row: what a failing assertion calls it, the claims the token carries, the
        /// action the configuration takes, the groups the token claims, the address the call
        /// came from, and the identity members those produce beside the claim set.
        type IdentityRow = (
            &'static str,
            Json,
            &'static str,
            &'static [&'static str],
            Option<&'static str>,
            Json,
        );
        let rows: &[IdentityRow] = &[
            (
                "an identity token of a user in two groups",
                ::serde_json::json!({
                    "sub": "user-1",
                    "iss": "https://issuer.test/pool",
                    "aud": "client-1",
                    "cognito:username": "alice",
                    "cognito:groups": ["Admins", "Readers"],
                }),
                "ALLOW",
                &["Admins", "Readers"],
                Some("127.0.0.1"),
                ::serde_json::json!({
                    "defaultAuthStrategy": "ALLOW",
                    "groups": ["Admins", "Readers"],
                    "issuer": "https://issuer.test/pool",
                    "sourceIp": ["127.0.0.1"],
                    "sub": "user-1",
                    "username": "alice",
                }),
            ),
            (
                "an access token of a user in no group, with no address stated",
                ::serde_json::json!({
                    "sub": "user-2",
                    "iss": "https://issuer.test/pool",
                    "client_id": "client-1",
                    "username": "bob",
                }),
                "DENY",
                &[],
                None,
                ::serde_json::json!({
                    "defaultAuthStrategy": "DENY",
                    "groups": Json::Null,
                    "issuer": "https://issuer.test/pool",
                    "sub": "user-2",
                    "username": "bob",
                }),
            ),
        ];
        for (case, claims, default_action, groups, source_ip, expected) in rows {
            let read = token(claims.clone());
            let groups: Vec<String> = groups.iter().map(|group| (*group).to_string()).collect();
            let actual = identity_of(
                &read,
                default_action,
                &groups,
                source_ip.map(str::to_string),
            );
            // The claim set itself is reported whole, beside the members derived from it.
            assert_eq!(
                actual.get("claims"),
                Some(claims),
                "case {case:?}: the identity must carry the token's own claim set, got {actual}"
            );
            let mut without_claims = actual.clone();
            if let Some(members) = without_claims.as_object_mut() {
                members.remove("claims");
            }
            assert_eq!(
                &without_claims, expected,
                "case {case:?}: expected {expected}, got {without_claims}"
            );
            assert_eq!(
                claimed_groups(&read),
                groups,
                "case {case:?}: the groups read must be the ones the token claims"
            );
        }
    }
}
