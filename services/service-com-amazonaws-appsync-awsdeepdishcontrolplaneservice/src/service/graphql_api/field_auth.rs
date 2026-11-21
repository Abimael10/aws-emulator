// SPDX-License-Identifier: CC0-1.0

//! Which AUTHORIZATION MODE a call on a GraphQL API resolved to, and which fields of the
//! API's schema that mode is granted.
//!
//! An API may set up more than one mode — "The authentication type" every call may use, plus
//! "a list of additional authentication providers for the GraphqlApi API"
//! (`GraphqlApi$authenticationType` / `$additionalAuthenticationProviders`) — and a schema
//! served by such an API says, per field, WHICH of those modes may reach it. The AppSync
//! developer guide's "additional authorization modes" section names one directive per mode
//! ([`MODE_DIRECTIVES`]): a field carrying some of them is reachable only by a call of one of
//! the modes they name, a field carrying none is reachable by the API's own
//! `authenticationType` — the mode every call may use — and a directive written on the TYPE
//! says the same about every field of it that writes none of its own.
//!
//! So the decision is a function of two things this module reads and nothing else: the mode
//! the call resolved to ([`resolved_mode`], taken once per call from the material it
//! presented and from what the token modes decided about it) and the directives the API's
//! stored schema writes ([`refuses_field`], asked field by field as the execution resolves
//! each one, because a directive is written on a field of ANY type and not only on the
//! operation's root).
//!
//! An API that set up ONE mode states no such directives, and every call on it is a call of
//! that one mode, so nothing here refuses any of its fields.
//!
//! Nothing is written: the API's own records are read under the one shared store lock, and a
//! call is not consumed by being judged.

use super::PlaneContext;
use crate::state::DirectiveUse;

/// The `errorType` AppSync reports a field the caller may not access under, and the sentence
/// it states: the field, and the TYPE THAT DECLARES it.
///
/// The ONE wording of that refusal, so every way a field is refused words it alike — the
/// authorization in front of the API, this module's mode directives, the Cognito group guard
/// and a resolver's own `util.unauthorized`.
pub const UNAUTHORIZED_FIELD_ERROR_TYPE: &str = "Unauthorized";

#[must_use]
pub fn unauthorized_field_message(type_name: &str, field_name: &str) -> String {
    format!("{UNAUTHORIZED_FIELD} {field_name} on type {type_name}")
}

/// The fixed opening of that sentence.
const UNAUTHORIZED_FIELD: &str = "Not Authorized to access";

/// The directive one authorization MODE is written with, paired with the
/// `AuthenticationType` it names — the pairs the AppSync developer guide lists for a schema
/// served under additional authorization modes.
///
/// `@aws_auth` is NOT among them: it is the group guard an API that set up the user pool mode
/// ALONE writes ([`super::user_pool`]), and it names groups rather than a mode.
pub const MODE_DIRECTIVES: [(&str, &str); 5] = [
    ("aws_api_key", "API_KEY"),
    ("aws_iam", "AWS_IAM"),
    ("aws_oidc", "OPENID_CONNECT"),
    ("aws_cognito_user_pools", "AMAZON_COGNITO_USER_POOLS"),
    ("aws_lambda", "AWS_LAMBDA"),
];

/// What the call PRESENTED, as the modes of an API read it: the pieces [`resolved_mode`]
/// needs, each taken where that mode's own decision was taken.
#[derive(Debug, Clone, Copy, Default)]
pub struct Presented<'a> {
    /// The API key the call presented, when it presented one.
    pub api_key: Option<&'a str>,
    /// The value of the call's own `authorization` field, when it carries one.
    pub authorization: Option<&'a str>,
    /// Whether the call presented SigV4 material that names a principal — what the `AWS_IAM`
    /// mode decides a call by ([`super::caller::presented_caller`]).
    pub signed: bool,
    /// Whether the user pool mode ADMITTED this call ([`super::user_pool::decided`]).
    pub user_pool_admitted: bool,
    /// Whether the API's Lambda authorizer ADMITTED this call ([`super::authorizer::decided`]).
    pub lambda_admitted: bool,
}

/// The authorization mode this call resolved to, or `None` when no mode the API set up
/// claims the material it presented.
///
/// Read in the order the modes claim material, which is the order the plane's own
/// authorization rule reads it in ([`super::authorized`]): an API key belongs to the
/// `API_KEY` mode, SigV4 material to `AWS_IAM`, and a token to whichever token mode decided
/// it — the user pool mode or the API's Lambda authorizer, each answered where that decision
/// was taken, and `OPENID_CONNECT` for a token an API that set that mode up presented when
/// neither of those claimed it. A mode the API did not SET UP claims nothing, so the answer
/// is always one of the API's own modes.
#[must_use]
pub fn resolved_mode(
    api: &crate::state::GraphqlApiRecord,
    presented: &Presented<'_>,
) -> Option<String> {
    let configured = |mode: &str| super::configures(api, mode);
    if presented.api_key.is_some() && configured(API_KEY_MODE) {
        return Some(API_KEY_MODE.to_string());
    }
    if presented.signed && configured(super::caller::IAM_AUTH_TYPE) {
        return Some(super::caller::IAM_AUTH_TYPE.to_string());
    }
    if presented.user_pool_admitted {
        return Some(super::user_pool::USER_POOL_AUTH_TYPE.to_string());
    }
    if presented.lambda_admitted {
        return Some(LAMBDA_MODE.to_string());
    }
    if presented.authorization.is_some_and(|value| !value.is_empty()) && configured(OIDC_MODE) {
        return Some(OIDC_MODE.to_string());
    }
    None
}

/// Whether the API's schema reserves `field_name` of `type_name` for authorization modes a
/// call of `mode` is NOT one of.
///
/// Asked field by field as the execution resolves each one, over BORROWED names, so a wide
/// selection costs one store read per field and no allocation of its own. The rule, per field:
///
/// * the modes the FIELD's own `@aws_*` directives name, where it writes any;
/// * otherwise the modes its TYPE's directives name, where the type writes any;
/// * otherwise the API's own `authenticationType`, the one mode every call may use.
///
/// A field whose modes do not include the call's own is a field the caller may not access: it
/// resolves to nothing and is reported as an error of its own path, with no resolver of it
/// run. A call that resolved NO mode is judged as a call of the API's own
/// `authenticationType` — the one mode every call may use — so it reaches exactly what a call
/// of that mode reaches and never more, whatever material the plane's own authorization rule
/// admitted it on ([`super::authorized`]). No field of an API that set up a SINGLE mode is
/// refused here: such a schema states no directives of this kind at all.
#[must_use]
pub fn refuses_field(
    context: &PlaneContext,
    type_name: &str,
    field_name: &str,
    mode: Option<&str>,
) -> bool {
    let store = context.state.lock();
    match store.graphql_apis.get(&context.api_id) {
        Some(api) => refuses_declared_field(api, type_name, field_name, mode),
        None => false,
    }
}

/// The same question asked of an API record a caller already holds under the store lock, so
/// a caller that reads the API for something else does not lock it again
/// ([`super::realtime`] reads the subscription root type beside this).
#[must_use]
pub fn refuses_declared_field(
    api: &crate::state::GraphqlApiRecord,
    type_name: &str,
    field_name: &str,
    mode: Option<&str>,
) -> bool {
    if !multi_mode(api) {
        return false;
    }
    let Some((on_field, on_type)) = written_directives(api, type_name, field_name) else {
        return false;
    };
    let named_on_field = named_modes(on_field);
    let named_on_type = match named_on_field.is_empty() {
        false => Vec::new(),
        true => named_modes(on_type),
    };
    let granted: &[&str] = match (named_on_field.is_empty(), named_on_type.is_empty()) {
        (false, _) => &named_on_field,
        (true, false) => &named_on_type,
        // A field under no directive at all, of a type under none: the one mode every call on
        // the API may use.
        (true, true) => &[api.authentication_type.as_str()],
    };
    !granted.contains(&mode.unwrap_or(api.authentication_type.as_str()))
}

/// The directives the API's schema writes on `field_name` of `type_name`, and the ones it
/// writes on the TYPE that declares it — `None` for a type the schema does not declare.
///
/// The ONE reading of WHERE a directive about a field is written, so the guards that judge
/// one cannot differ about it: the authorization modes a field is reserved for
/// ([`refuses_field`]) and the Cognito groups it is guarded by
/// ([`super::user_pool::refused_fields`]) are each a predicate over these two lists, with the
/// type's taken where the field's states nothing of that kind.
pub fn written_directives<'api>(
    api: &'api crate::state::GraphqlApiRecord,
    type_name: &str,
    field_name: &str,
) -> Option<(&'api [DirectiveUse], &'api [DirectiveUse])> {
    let record = api.types.get(type_name)?;
    let on_field = record
        .parsed
        .fields
        .iter()
        .find(|field| field.name == field_name)
        .map_or(&[][..], |field| field.directives.as_slice());
    Some((on_field, record.parsed.directives.as_slice()))
}

/// Whether `api` set up more than ONE authorization mode, which is the API whose schema says
/// per field which of them may reach it: one holding an additional authentication provider
/// that names a type at all, whichever type that is.
///
/// A provider that NAMES no authentication type sets none up, which is the same reading
/// [`super::configures`] gives the question.
fn multi_mode(api: &crate::state::GraphqlApiRecord) -> bool {
    api.additional_authentication_providers
        .iter()
        .any(|provider| provider.authentication_type.is_some())
}

/// The authorization modes the `@aws_*` directives among `directives` name, in the order they
/// are written — empty for directives that name none, which states nothing about which mode
/// may reach what they are written on.
fn named_modes(directives: &[DirectiveUse]) -> Vec<&'static str> {
    directives
        .iter()
        .filter_map(|directive| {
            MODE_DIRECTIVES
                .iter()
                .find(|(name, _)| *name == directive.name)
                .map(|(_, mode)| *mode)
        })
        .collect()
}

/// The modes named above that are not spelled out by a module of their own.
const API_KEY_MODE: &str = "API_KEY";
const OIDC_MODE: &str = "OPENID_CONNECT";
const LAMBDA_MODE: &str = "AWS_LAMBDA";
