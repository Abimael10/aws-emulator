// SPDX-License-Identifier: CC0-1.0

//! The `AWS_LAMBDA` authorization mode of a GraphQL API: the decision taken BEFORE any field
//! of a request resolves.
//!
//! "A `LambdaAuthorizerConfig` specifies how to authorize AppSync API access when using the
//! `AWS_LAMBDA` authorizer mode" — so for an API that configured one, the function
//! `authorizerUri` names is invoked with the token the request presented, and what it RETURNED
//! decides the request:
//!
//! * `isAuthorized` — a call the authorizer did not authorize reaches no resolver at all, and
//!   is answered the refusal of this mode;
//! * `deniedFields` — the fields the authorizer withheld from this caller, each named either
//!   by the API's own `…/types/<typeName>/fields/<fieldName>` ARN or by the short
//!   `<TypeName>.<fieldName>`; they resolve to nothing and are reported as fields the caller
//!   may not access, and every other field the call selects resolves;
//! * `resolverContext` — what the authorizer published about the caller, which is what a
//!   resolver of this API reads as `$context.identity.resolverContext`;
//! * `ttlOverride` — how long THIS decision may be reused, which the function states for
//!   itself: "The Lambda function can override this by returning a `ttlOverride` key in its
//!   response" (`LambdaAuthorizerConfig$authorizerResultTtlInSeconds`). The configured value
//!   is the default, and "the default is 0 seconds, which disables caching", so a decision is
//!   reused only while a positive window of it is live
//!   ([`crate::state::lambda_authorizer_cache`]).
//!
//! `identityValidationExpression` is "a regular expression for validation of tokens before the
//! Lambda function is called", so a token it does not match is refused HERE: no invocation is
//! made for it at all.
//!
//! The invocation is the ONE in-process hop this plane makes to AWS Lambda
//! ([`super::lambda`]), so the decision is the one the function itself returned.

use ::std::time::{Duration, Instant};

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::lambda;
use super::PlaneContext;
use crate::ops::types::LambdaAuthorizerConfig;

/// The authorization MODE this module decides for: `AWS_LAMBDA` of `AuthenticationType`.
pub const LAMBDA_AUTH_TYPE: &str = "AWS_LAMBDA";

/// The members the authorizer's answer carries.
const IS_AUTHORIZED_MEMBER: &str = "isAuthorized";
const RESOLVER_CONTEXT_MEMBER: &str = "resolverContext";
const DENIED_FIELDS_MEMBER: &str = "deniedFields";
const TTL_OVERRIDE_MEMBER: &str = "ttlOverride";

/// The member a resolver reads what the authorizer published under
/// (`$context.identity.resolverContext`).
const IDENTITY_RESOLVER_CONTEXT: &str = "resolverContext";

/// The segments the ARN form of a denied field names the type and the field by:
/// "arn:aws:appsync:<region>:<account>:apis/<apiId>/types/<TypeName>/fields/<FieldName>".
const TYPES_SEGMENT: &str = "types";
const FIELDS_SEGMENT: &str = "fields";

/// What this mode decided about one request.
#[derive(Debug, Clone)]
pub enum Decided {
    /// The mode does not decide this request: the API configured no Lambda authorizer, or the
    /// call presented material of another provider this API also configured.
    NotApplicable,
    /// The call reaches no field of the API, and nothing of it was run.
    Refused,
    /// The call is served, under what the authorizer published about it.
    Admitted {
        /// What a resolver reads as `$context.identity` for this caller.
        identity: Json,
        /// The `(typeName, fieldName)` pairs the authorizer withheld from it.
        denied: Vec<(String, String)>,
    },
}

/// What one request carries that the authorizer's event reports about it.
#[derive(Debug, Clone)]
pub struct Presented<'a> {
    /// The material the request presented in its own `authorization` field, which is where
    /// AppSync reads an authorizer's input from.
    pub token: &'a str,
    /// The id this endpoint answered the request under.
    pub request_id: &'a str,
    /// The GraphQL document the request carried.
    pub query: &'a str,
    /// The operation of it the request named, where it named one.
    pub operation_name: Option<&'a str>,
    /// The variables that document is parameterised by.
    pub variables: &'a JsonMap<String, Json>,
}

/// Decide `presented` under the `AWS_LAMBDA` mode of the API `context` serves.
///
/// A request is decided by this mode only where the API CONFIGURED it — as its
/// `authenticationType` or as one of its `additionalAuthenticationProviders`, which is the one
/// reading of that question this service has ([`super::configures`]) — and only where the
/// material presented is this mode's. A SIGNATURE is the `AWS_IAM` mode's material, so a signed
/// call is left to that mode wherever the API configured it; an API that did not configure it
/// set up no mode that decides a signature, so what such a call presented is this mode's input
/// like any other. A call presenting nothing in that field is decided by whichever mode reads
/// the material it did present.
pub async fn decided(context: &PlaneContext, presented: &Presented<'_>) -> Decided {
    let Some(configured) = authorizer_config(context) else {
        return Decided::NotApplicable;
    };
    if presented.token.is_empty() || decided_by_signature(context, presented.token) {
        return Decided::NotApplicable;
    }
    let uri = configured.authorizer_uri.as_str();
    if uri.is_empty() {
        return Decided::NotApplicable;
    }
    // "A regular expression for validation of tokens before the Lambda function is called": a
    // token the expression does not match is refused with no invocation made for it.
    if let Some(expression) = configured
        .identity_validation_expression
        .as_deref()
        .filter(|expression| !expression.is_empty())
    {
        if !validated(expression, presented.token) {
            ::tracing::info!(
                api_id = %context.api_id,
                "this API's identityValidationExpression does not match the token presented, \
                 so its Lambda authorizer was not invoked"
            );
            return Decided::Refused;
        }
    }
    // A decision already taken for this token, while the window it was taken for is live.
    let cached = context
        .state
        .lock()
        .lambda_authorizer_cache
        .cached(&context.api_id, presented.token, Instant::now());
    let answer = match cached {
        Some(answer) => answer,
        None => {
            let Some(target) = lambda::function_target(uri) else {
                ::tracing::warn!(
                    api_id = %context.api_id,
                    "this API's authorizerUri names no Lambda function, so no authorization \
                     decision was taken"
                );
                return Decided::Refused;
            };
            let event = authorizer_event(context, presented);
            let ran = match lambda::invoke(&context.callback, &target, &event).await {
                Ok(ran) => ran,
                Err(unreached) => {
                    ::tracing::warn!(
                        api_id = %context.api_id,
                        reason = %unreached.reason(),
                        "this API's Lambda authorizer answered nothing, so the call reaches no \
                         field of it"
                    );
                    return Decided::Refused;
                }
            };
            if ran.failed() {
                let (class, message) = ran.raised();
                ::tracing::info!(
                    api_id = %context.api_id,
                    %class,
                    %message,
                    "this API's Lambda authorizer raised, so the call reaches no field of it"
                );
                return Decided::Refused;
            }
            let answer = ran.value();
            // The window the decision may be reused for is the function's own where it stated
            // one, else the configuration's; a zero window caches nothing.
            let window = cacheable_window(&answer, configured.authorizer_result_ttl_in_seconds);
            if !window.is_zero() {
                context.state.lock().lambda_authorizer_cache.remember(
                    &context.api_id,
                    presented.token,
                    &answer,
                    window,
                    Instant::now(),
                );
            }
            answer
        }
    };
    if !answer
        .get(IS_AUTHORIZED_MEMBER)
        .and_then(Json::as_bool)
        .unwrap_or(false)
    {
        return Decided::Refused;
    }
    Decided::Admitted {
        identity: identity_of(&answer),
        denied: denied_fields(&answer),
    }
}

/// Whether `token` is material the `AWS_IAM` mode of this API decides instead: a SigV4
/// signature, on an API that configured that mode.
///
/// The two modes read different fields of the same request, so which of them decides a call is
/// what the call PRESENTED — and a signature is decided by the caller's own IAM policies
/// ([`super::caller`]) rather than by an authorizer. An API that did not configure `AWS_IAM`
/// set up no mode that reads a signature at all, so nothing of such a call is left to one.
fn decided_by_signature(context: &PlaneContext, token: &str) -> bool {
    token.starts_with(SIGV4_SCHEME)
        && context
            .state
            .lock()
            .graphql_apis
            .get(&context.api_id)
            .is_some_and(|api| super::configures(api, super::caller::IAM_AUTH_TYPE))
}

/// The scheme a SigV4-signed request presents its material under.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The Lambda authorizer configuration in force on the API `context` serves, or `None` for an
/// API that configured none.
///
/// The mode has to be SET UP at all — as the API's `authenticationType` or as one of its
/// `additionalAuthenticationProviders`, which is the one reading of that question this service
/// has ([`super::configures`]) — and "an AppSync API can have only one Lambda authorizer
/// configured at a time" (`LambdaAuthorizerConfig`), so whichever of the two places carries one
/// IS that authorizer: the provider's own where it carries one, and the API's own otherwise.
fn authorizer_config(context: &PlaneContext) -> Option<LambdaAuthorizerConfig> {
    let store = context.state.lock();
    let api = store.graphql_apis.get(&context.api_id)?;
    if !super::configures(api, LAMBDA_AUTH_TYPE) {
        return None;
    }
    api.additional_authentication_providers
        .iter()
        .filter(|provider| provider.authentication_type.as_deref() == Some(LAMBDA_AUTH_TYPE))
        .find_map(|provider| provider.lambda_authorizer_config.clone())
        .or_else(|| api.lambda_authorizer_config.clone())
}

/// The event the authorizer is invoked with: the token the request presented, and what the
/// request itself was.
fn authorizer_event(context: &PlaneContext, presented: &Presented<'_>) -> Json {
    let mut request_context = JsonMap::new();
    request_context.insert("apiId".to_string(), json!(context.api_id));
    request_context.insert(
        "accountId".to_string(),
        json!(::engine::emulator_account_id()),
    );
    request_context.insert("requestId".to_string(), json!(presented.request_id));
    request_context.insert("queryString".to_string(), json!(presented.query));
    request_context.insert(
        "operationName".to_string(),
        presented
            .operation_name
            .map_or(Json::Null, |named| json!(named)),
    );
    request_context.insert(
        "variables".to_string(),
        Json::Object(presented.variables.clone()),
    );
    json!({
        "authorizationToken": presented.token,
        "requestContext": Json::Object(request_context),
    })
}

/// Whether `token` satisfies the API's `identityValidationExpression`.
///
/// The expression validates the token, so it has to describe the WHOLE of it rather than some
/// part of it: a token is admitted only where the expression matches it end to end. The
/// expression comes from a stored configuration rather than from this crate, so it is compiled
/// under the explicit program budget this crate compiles every caller-supplied pattern under,
/// and one past that budget — or one that is not a pattern at all — admits no token.
fn validated(expression: &str, token: &str) -> bool {
    let anchored = format!("^(?:{expression})$");
    let compiled = ::regex::RegexBuilder::new(&anchored)
        .size_limit(super::mapping_template::utilities::MAX_PATTERN_PROGRAM_BYTES)
        .dfa_size_limit(super::mapping_template::utilities::MAX_PATTERN_DFA_BYTES)
        .build();
    match compiled {
        Ok(pattern) => pattern.is_match(token),
        Err(_) => false,
    }
}

/// How long the decision `answer` may be REUSED: the window the function stated for itself
/// (`ttlOverride`) where it stated one, else the window the API configured
/// (`authorizerResultTtlInSeconds`). "The default is 0 seconds, which disables caching", so a
/// negative or absent value is no window at all.
fn cacheable_window(answer: &Json, configured: i32) -> Duration {
    let seconds = answer
        .get(TTL_OVERRIDE_MEMBER)
        .and_then(Json::as_i64)
        .unwrap_or(i64::from(configured));
    Duration::from_secs(u64::try_from(seconds.max(0)).unwrap_or(0))
}

/// What a resolver reads as `$context.identity` for a call this mode admitted: what the
/// authorizer published about the caller, under the member a resolver reads it in.
fn identity_of(answer: &Json) -> Json {
    let published = answer
        .get(RESOLVER_CONTEXT_MEMBER)
        .cloned()
        .unwrap_or_else(|| Json::Object(JsonMap::new()));
    json!({ IDENTITY_RESOLVER_CONTEXT: published })
}

/// The `(typeName, fieldName)` pairs `answer` withheld from the caller.
///
/// Each entry is either the API's own field ARN or the short `<TypeName>.<fieldName>`, and both
/// name one field of one type, so both are read the same way: the ARN by the `types`/`fields`
/// segments it names them in, the short form by the dot between them. An entry that names
/// neither withholds nothing.
fn denied_fields(answer: &Json) -> Vec<(String, String)> {
    answer
        .get(DENIED_FIELDS_MEMBER)
        .and_then(Json::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Json::as_str)
                .filter_map(denied_field)
                .collect()
        })
        .unwrap_or_default()
}

/// The `(typeName, fieldName)` one entry of `deniedFields` names, or `None` for one that names
/// no field.
fn denied_field(entry: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = entry.split('/').collect();
    if let Some(at) = segments
        .iter()
        .position(|segment| *segment == TYPES_SEGMENT)
    {
        let type_name = segments.get(at.saturating_add(1))?;
        let keyword = segments.get(at.saturating_add(2))?;
        let field_name = segments.get(at.saturating_add(3))?;
        if *keyword == FIELDS_SEGMENT && !type_name.is_empty() && !field_name.is_empty() {
            return Some((type_name.to_string(), field_name.to_string()));
        }
        return None;
    }
    let (type_name, field_name) = entry.split_once('.')?;
    (!type_name.is_empty() && !field_name.is_empty())
        .then(|| (type_name.to_string(), field_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token is admitted only where the expression describes the WHOLE of it, and an
    /// expression this crate cannot compile admits none.
    #[test]
    fn a_token_is_validated_against_the_whole_expression() {
        // The rows differ along the FORM of the expression: anchored and unanchored, a
        // character class, an alternation, one that matches only part of the token, and one
        // that is not a pattern at all.
        let cases: [(&str, &str, &str, bool); 8] = [
            ("an anchored expression the token matches", "^custom-.*$", "custom-token", true),
            ("the same expression and another token", "^custom-.*$", "other-token", false),
            ("an unanchored expression the token matches whole", "custom-.*", "custom-token", true),
            (
                "an unanchored expression matching only part of the token",
                "custom-",
                "prefix-custom-token",
                false,
            ),
            ("a character class", "[a-z0-9-]{4,32}", "abc-123", true),
            ("a character class the token leaves", "[a-z0-9-]{4,32}", "ABC", false),
            ("an alternation", "allow|permit", "permit", true),
            ("an expression that is not a pattern", "custom-[", "custom-token", false),
        ];
        for (case, expression, token, expected) in cases {
            let actual = validated(expression, token);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// Both spellings `deniedFields` documents name one field of one type, and anything else
    /// names none.
    #[test]
    fn every_spelling_of_a_denied_field_names_its_type_and_field() {
        /// One row: what a failing assertion calls it, the entry read, and the
        /// `(typeName, fieldName)` it names.
        type Entry = (&'static str, &'static str, Option<(&'static str, &'static str)>);

        let cases: [Entry; 6] = [
            (
                "the fully qualified field ARN",
                "arn:aws:appsync:us-east-1:000000000000:apis/anapiid/types/Query/fields/secret",
                Some(("Query", "secret")),
            ),
            (
                "the short TypeName.fieldName form",
                "Query.secret",
                Some(("Query", "secret")),
            ),
            (
                "a mutation's field in the short form",
                "Mutation.addNote",
                Some(("Mutation", "addNote")),
            ),
            (
                "an ARN that names a type and no field",
                "arn:aws:appsync:us-east-1:000000000000:apis/anapiid/types/Query",
                None,
            ),
            ("an entry naming no field at all", "Query", None),
            ("an empty entry", "", None),
        ];
        for (case, entry, expected) in cases {
            let actual = denied_field(entry);
            let expected = expected.map(|(type_name, field_name)| {
                (type_name.to_string(), field_name.to_string())
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The window a decision may be reused for is the function's own where it stated one, and
    /// the configuration's where it did not; neither can be negative.
    #[test]
    fn the_reuse_window_is_the_functions_own_where_it_stated_one() {
        let cases: [(&str, Json, i32, u64); 6] = [
            ("no override and no configured window", json!({}), 0, 0),
            ("no override, a configured window", json!({}), 300, 300),
            (
                "an override that disables caching",
                json!({"ttlOverride": 0}),
                300,
                0,
            ),
            (
                "an override shorter than the configured window",
                json!({"ttlOverride": 30}),
                300,
                30,
            ),
            (
                "an override longer than the configured window",
                json!({"ttlOverride": 3600}),
                300,
                3600,
            ),
            (
                "an override that is not a window",
                json!({"ttlOverride": -1}),
                300,
                0,
            ),
        ];
        for (case, answer, configured, expected) in cases {
            let actual = cacheable_window(&answer, configured).as_secs();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected} seconds, got {actual}"
            );
        }
    }
}
