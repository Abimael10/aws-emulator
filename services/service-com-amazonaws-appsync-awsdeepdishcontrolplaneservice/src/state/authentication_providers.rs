// SPDX-License-Identifier: CC0-1.0

//! The `additionalAuthenticationProviders` a GraphQL API is configured with: what makes
//! two of them the SAME way of authenticating the API, and how many of them one API holds.
//!
//! `additionalAuthenticationProviders` targets a list of
//! `AdditionalAuthenticationProvider{authenticationType, openIDConnectConfig,
//! userPoolConfig, lambdaAuthorizerConfig}` with no modelled list bound
//! (`smithy_query(query="operation", operation="CreateGraphqlApi,UpdateGraphqlApi")`), so
//! both rules below are the service's rather than the boundary's, and both are carried by
//! `BadRequestException` (HTTP 400), the client error either operation models:
//!
//! * a provider is ONE way of authenticating the API, and what identifies that way is the
//!   provider's CONFIGURATION — the issuer an OIDC provider's ID tokens must carry, the
//!   Cognito user pool a user-pool provider admits tokens of, the authorizer function a
//!   Lambda provider calls — so an API may be given several providers of one
//!   `authenticationType` as long as each names a different configuration, and only a
//!   provider that repeats a configuration the API already has is refused. A type set up
//!   through no configuration at all (`API_KEY`, `AWS_IAM`:
//!   [`super::missing_auth_config`] names the ones that need one) is identified by the
//!   type alone, so it can be named once;
//! * an API holds at most [`ADDITIONAL_AUTH_PROVIDERS_PER_API`] additional providers, and
//!   a request carrying more is refused with the quota and the number it carried
//!   ([`additional_auth_providers_limit_message`]).
//!
//! Shared by the create and the update, which carry the same member set and are refused on
//! the same grounds ([`super::graphql_api_config_refusal`]).

use std::collections::BTreeSet;

use crate::ops::types::{
    AdditionalAuthenticationProvider, LambdaAuthorizerConfig, OpenIDConnectConfig, UserPoolConfig,
};
use crate::service::graphql_api::authorizer::LAMBDA_AUTH_TYPE;
use crate::service::graphql_api::open_id_connect::OPEN_ID_CONNECT_AUTH_TYPE;
use crate::service::graphql_api::user_pool::USER_POOL_AUTH_TYPE;

/// AppSync's quota for the additional authentication providers of ONE GraphQL API, which
/// also bounds the list a create or an update may carry: a request past it is refused, so
/// no stored API holds more than this many.
pub const ADDITIONAL_AUTH_PROVIDERS_PER_API: usize = 50;

/// The text `BadRequestException` (HTTP 400) carries for a request carrying more
/// additional authentication providers than an API holds: the quota, and the number of
/// providers the request carried, so the caller can see how far past it the request went.
pub fn additional_auth_providers_limit_message(submitted: usize) -> String {
    format!(
        "Maximum number of additional authentication providers is \
         {ADDITIONAL_AUTH_PROVIDERS_PER_API}. You added {submitted}."
    )
}

/// The text `BadRequestException` (HTTP 400) carries for a provider that repeats a way of
/// authenticating the API the request already configured.
///
/// The refusal names the provider by its 1-based POSITION in the list, the only way a
/// caller can tell two entries apart, and by the type it set up.
pub fn duplicate_auth_provider_message(authentication_type: &str, position: usize) -> String {
    format!(
        "Authentication type {authentication_type} for additional authentication provider \
         {position} already specified on the API. It can only be specified once."
    )
}

/// What identifies ONE way of authenticating an API: its authentication type together with
/// the configuration that type is set up through, which is what tells two providers of the
/// same type apart.
///
/// The configuration member a type is set up through is the one
/// [`super::missing_auth_config`] requires for it, and the value inside it that identifies
/// the authority behind the mode: an OIDC provider's `issuer` ("must exactly match the
/// value of `iss` in the ID token"), a user-pool provider's `userPoolId` and a Lambda
/// provider's `authorizerUri`. A type configured by nothing is identified by the type.
fn auth_provider_identity(
    authentication_type: &str,
    open_id_connect_issuer: Option<&str>,
    user_pool_id: Option<&str>,
    authorizer_uri: Option<&str>,
) -> (String, String) {
    let configuration = match authentication_type {
        OPEN_ID_CONNECT_AUTH_TYPE => open_id_connect_issuer,
        USER_POOL_AUTH_TYPE => user_pool_id,
        LAMBDA_AUTH_TYPE => authorizer_uri,
        _ => None,
    };
    (
        authentication_type.to_string(),
        configuration.unwrap_or_default().to_string(),
    )
}

/// Why the `additionalAuthenticationProviders` list a request carries is not one an API
/// can hold, as the text a `BadRequestException` (HTTP 400) states it with, or `None`
/// when it is.
///
/// The two rules this module documents, in the order the service reads them: the list's
/// LENGTH against the quota first, because a list past the quota cannot be configured
/// however its entries are spelled, then each entry against the ways of authenticating the
/// API already configured — the API's OWN authentication type and configuration included,
/// since that is one of them.
pub fn additional_auth_providers_refusal(
    authentication_type: &str,
    open_id_connect_config: Option<&OpenIDConnectConfig>,
    user_pool_config: Option<&UserPoolConfig>,
    lambda_authorizer_config: Option<&LambdaAuthorizerConfig>,
    additional_authentication_providers: &[AdditionalAuthenticationProvider],
) -> Option<String> {
    if additional_authentication_providers.len() > ADDITIONAL_AUTH_PROVIDERS_PER_API {
        return Some(additional_auth_providers_limit_message(
            additional_authentication_providers.len(),
        ));
    }

    // The set holds one entry per way of authenticating the API the request configures,
    // and the request is refused as soon as one repeats, so it never grows past the
    // quota checked above plus the API's own mode.
    let mut specified: BTreeSet<(String, String)> = BTreeSet::from([auth_provider_identity(
        authentication_type,
        open_id_connect_config.map(|config| config.issuer.as_str()),
        user_pool_config.map(|config| config.user_pool_id.as_str()),
        lambda_authorizer_config.map(|config| config.authorizer_uri.as_str()),
    )]);
    for (position, provider) in additional_authentication_providers.iter().enumerate() {
        let Some(provider_type) = provider.authentication_type.as_deref() else {
            continue;
        };
        let identity = auth_provider_identity(
            provider_type,
            provider
                .open_id_connect_config
                .as_ref()
                .map(|config| config.issuer.as_str()),
            provider
                .user_pool_config
                .as_ref()
                .map(|config| config.user_pool_id.as_str()),
            provider
                .lambda_authorizer_config
                .as_ref()
                .map(|config| config.authorizer_uri.as_str()),
        );
        if !specified.insert(identity) {
            return Some(duplicate_auth_provider_message(provider_type, position + 1));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::CognitoUserPoolConfig;

    /// One additional provider of `authentication_type`, configured by `configuration`
    /// through the member that type is set up through.
    fn provider(authentication_type: &str, configuration: &str) -> AdditionalAuthenticationProvider {
        let mut entry = AdditionalAuthenticationProvider {
            authentication_type: Some(authentication_type.to_string()),
            ..Default::default()
        };
        match authentication_type {
            OPEN_ID_CONNECT_AUTH_TYPE => {
                entry.open_id_connect_config = Some(OpenIDConnectConfig {
                    issuer: configuration.to_string(),
                    ..Default::default()
                });
            }
            USER_POOL_AUTH_TYPE => {
                entry.user_pool_config = Some(CognitoUserPoolConfig {
                    user_pool_id: configuration.to_string(),
                    ..Default::default()
                });
            }
            LAMBDA_AUTH_TYPE => {
                entry.lambda_authorizer_config = Some(LambdaAuthorizerConfig {
                    authorizer_uri: configuration.to_string(),
                    ..Default::default()
                });
            }
            _ => {}
        }
        entry
    }

    /// RULE: providers of one type with DIFFERENT configurations are different ways of
    /// authenticating the API, up to the quota.
    #[test]
    fn distinct_configurations_of_one_type_are_accepted_up_to_the_quota() {
        for authentication_type in [
            OPEN_ID_CONNECT_AUTH_TYPE,
            USER_POOL_AUTH_TYPE,
            LAMBDA_AUTH_TYPE,
        ] {
            for count in [1, 2, ADDITIONAL_AUTH_PROVIDERS_PER_API] {
                let providers: Vec<AdditionalAuthenticationProvider> = (0..count)
                    .map(|index| provider(authentication_type, &format!("c-{index}")))
                    .collect();
                let refusal =
                    additional_auth_providers_refusal("API_KEY", None, None, None, &providers);
                assert!(
                    refusal.is_none(),
                    "case {count} {authentication_type} providers: expected no refusal, got \
                     {refusal:?}"
                );
            }
        }
    }

    /// RULE: a provider repeating a configuration the API already has is refused, naming
    /// its position.
    #[test]
    fn a_repeated_configuration_is_refused_by_position() {
        for authentication_type in [
            OPEN_ID_CONNECT_AUTH_TYPE,
            USER_POOL_AUTH_TYPE,
            LAMBDA_AUTH_TYPE,
        ] {
            let providers = vec![
                provider(authentication_type, "first"),
                provider(authentication_type, "second"),
                provider(authentication_type, "first"),
            ];
            let refusal = additional_auth_providers_refusal("API_KEY", None, None, None, &providers);
            let expected = duplicate_auth_provider_message(authentication_type, 3);
            assert_eq!(
                refusal.as_deref(),
                Some(expected.as_str()),
                "case a repeated {authentication_type} configuration: expected {expected:?}, got \
                 {refusal:?}"
            );
        }
    }

    /// RULE: a type set up through no configuration is identified by the type alone, so
    /// naming it twice — or naming the API's own mode again — repeats one way of
    /// authenticating the API.
    #[test]
    fn a_type_configured_by_nothing_can_be_named_once() {
        for authentication_type in ["API_KEY", "AWS_IAM"] {
            let providers = vec![
                provider(authentication_type, ""),
                provider(authentication_type, ""),
            ];
            let refusal = additional_auth_providers_refusal(
                "AMAZON_COGNITO_USER_POOLS",
                None,
                Some(&UserPoolConfig {
                    user_pool_id: "pool".to_string(),
                    ..Default::default()
                }),
                None,
                &providers,
            );
            let expected = duplicate_auth_provider_message(authentication_type, 2);
            assert_eq!(
                refusal.as_deref(),
                Some(expected.as_str()),
                "case {authentication_type} named twice: expected {expected:?}, got {refusal:?}"
            );
        }

        // The API's own mode is one of the ways it is authenticated, so a provider that
        // repeats its configuration is a duplicate too.
        let providers = vec![provider(OPEN_ID_CONNECT_AUTH_TYPE, "https://a.example.com")];
        let refusal = additional_auth_providers_refusal(
            OPEN_ID_CONNECT_AUTH_TYPE,
            Some(&OpenIDConnectConfig {
                issuer: "https://a.example.com".to_string(),
                ..Default::default()
            }),
            None,
            None,
            &providers,
        );
        let expected = duplicate_auth_provider_message(OPEN_ID_CONNECT_AUTH_TYPE, 1);
        assert_eq!(
            refusal.as_deref(),
            Some(expected.as_str()),
            "case a provider repeating the API's own issuer: expected {expected:?}, got {refusal:?}"
        );
    }

    /// RULE: a list one entry past the quota is refused with the quota and the number of
    /// providers the request carried.
    #[test]
    fn a_list_past_the_quota_is_refused_with_the_count() {
        for count in [
            ADDITIONAL_AUTH_PROVIDERS_PER_API + 1,
            ADDITIONAL_AUTH_PROVIDERS_PER_API + 9,
        ] {
            let providers: Vec<AdditionalAuthenticationProvider> = (0..count)
                .map(|index| {
                    provider(
                        OPEN_ID_CONNECT_AUTH_TYPE,
                        &format!("https://issuer-{index}.example.com"),
                    )
                })
                .collect();
            let refusal = additional_auth_providers_refusal("API_KEY", None, None, None, &providers);
            let expected = additional_auth_providers_limit_message(count);
            assert_eq!(
                refusal.as_deref(),
                Some(expected.as_str()),
                "case {count} providers: expected {expected:?}, got {refusal:?}"
            );
        }
    }
}
