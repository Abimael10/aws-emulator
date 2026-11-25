// SPDX-License-Identifier: CC0-1.0

//! The `additionalAuthenticationProviders` of a GraphQL API, end to end over HTTP: what
//! makes two of them the same way of authenticating the API, and how many of them one API
//! holds.
//!
//! `additionalAuthenticationProviders` targets a list of
//! `AdditionalAuthenticationProvider{authenticationType, openIDConnectConfig,
//! userPoolConfig, lambdaAuthorizerConfig}` with no modelled list bound
//! (`smithy_query(query="operation", operation="CreateGraphqlApi,UpdateGraphqlApi")`), and
//! both operations model `BadRequestException` (400). Two rules are asserted here, each
//! over a TABLE spanning the class it generalises over rather than one recorded request:
//!
//! 1. a provider is ONE way of authenticating the API, identified by the CONFIGURATION it
//!    names — an OIDC issuer, a Cognito user pool id, a Lambda authorizer URI — so an API
//!    accepts several providers of one `authenticationType` as long as each names a
//!    different configuration, and only a provider repeating a configuration the API
//!    already has (the API's own authentication mode included) is refused, by its 1-based
//!    position. A type set up through no configuration at all (`API_KEY`, `AWS_IAM`) is
//!    identified by the type, so it can be named once;
//! 2. an API holds at most 50 additional providers: a list at the quota is stored and read
//!    back, a list past it is refused with the quota and the number the request carried,
//!    and a refused update leaves the stored providers in place — a later same-shape
//!    update at the quota still succeeds.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the handlers and the shared engine
//! together. The harness (`call`, `router`, `Answer`) is the error-contract suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, router, Answer};

/// AppSync's quota for the additional authentication providers of one API, which the
/// service's own refusal names.
const PROVIDERS_PER_API: usize = 50;

/// The authentication types set up THROUGH a configuration member of their own, each
/// beside the member that carries it and the member inside it that identifies the
/// authority behind the mode. Source: the `AdditionalAuthenticationProvider`,
/// `OpenIDConnectConfig`, `CognitoUserPoolConfig` and `LambdaAuthorizerConfig` shapes
/// (`smithy_query(query="shape",
/// shape="AdditionalAuthenticationProvider,OpenIDConnectConfig,CognitoUserPoolConfig,LambdaAuthorizerConfig")`).
const CONFIGURED_TYPES: &[(&str, &str, &str)] = &[
    ("OPENID_CONNECT", "openIDConnectConfig", "issuer"),
    ("AMAZON_COGNITO_USER_POOLS", "userPoolConfig", "userPoolId"),
    ("AWS_LAMBDA", "lambdaAuthorizerConfig", "authorizerUri"),
];

/// The authentication types set up through NO configuration member, which a provider can
/// therefore name only once.
const UNCONFIGURED_TYPES: &[&str] = &["API_KEY", "AWS_IAM"];

/// One additional provider of `authentication_type` whose configuration is identified by
/// `identity`, with every member that type's configuration shape requires.
fn provider(authentication_type: &str, identity: &str) -> Value {
    match authentication_type {
        "OPENID_CONNECT" => json!({
            "authenticationType": authentication_type,
            "openIDConnectConfig": {"issuer": format!("https://{identity}.example.com")},
        }),
        "AMAZON_COGNITO_USER_POOLS" => json!({
            "authenticationType": authentication_type,
            "userPoolConfig": {"userPoolId": identity, "awsRegion": "eu-west-1"},
        }),
        "AWS_LAMBDA" => json!({
            "authenticationType": authentication_type,
            "lambdaAuthorizerConfig": {
                "authorizerUri":
                    format!("arn:aws:lambda:eu-west-1:000000000000:function:{identity}"),
            },
        }),
        _ => json!({"authenticationType": authentication_type}),
    }
}

/// The value inside a provider's configuration that identifies it, as the reply reports
/// it — the member named in [`CONFIGURED_TYPES`].
fn reported_identity(reported: &Value, authentication_type: &str) -> String {
    CONFIGURED_TYPES
        .iter()
        .find(|(name, ..)| *name == authentication_type)
        .map(|(_, config, member)| reported[config][member].to_string())
        .unwrap_or_default()
}

/// `count` providers of `authentication_type`, each naming a configuration of its own.
fn distinct_providers(authentication_type: &str, count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| provider(authentication_type, &format!("p-{index}")))
        .collect()
}

/// A `CreateGraphqlApi`/`UpdateGraphqlApi` body carrying `providers`, with the members
/// both operations require.
fn api_body(name: &str, providers: &[Value]) -> Value {
    json!({
        "name": name,
        "authenticationType": "API_KEY",
        "queryDepthLimit": 0,
        "resolverCountLimit": 0,
        "xrayEnabled": false,
        "additionalAuthenticationProviders": providers,
    })
}

/// Create one GraphQL API carrying `providers` and return the answer.
async fn create_api_with(router: &MultiServiceRouter, name: &str, providers: &[Value]) -> Answer {
    call(router, "POST", "/v1/apis", Some(api_body(name, providers))).await
}

/// Update the API `api_id` to carry `providers` and return the answer.
async fn update_api_with(
    router: &MultiServiceRouter,
    api_id: &str,
    name: &str,
    providers: &[Value],
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}"),
        Some(api_body(name, providers)),
    )
    .await
}

/// Read the API `api_id` back.
async fn get_api(router: &MultiServiceRouter, api_id: &str) -> Answer {
    call(router, "GET", &format!("/v1/apis/{api_id}"), None).await
}

/// The providers an API reply reports, as a list.
fn reported_providers(answer: &Answer, member: &str) -> Vec<Value> {
    answer.body[member]["additionalAuthenticationProviders"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Rule 1: providers of ONE type with distinct configurations are distinct ways of
/// authenticating the API — accepted by the create and by the update, and read back in
/// full.
#[tokio::test]
async fn distinct_configurations_of_one_type_are_accepted_and_read_back() {
    let router = router();

    for (authentication_type, ..) in CONFIGURED_TYPES {
        for count in [1_usize, 2, 7, PROVIDERS_PER_API] {
            let providers = distinct_providers(authentication_type, count);
            let case = format!("{count} {authentication_type} providers");

            let created = create_api_with(
                &router,
                &format!("Api_create_{authentication_type}_{count}"),
                &providers,
            )
            .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected CreateGraphqlApi 200, got {created:?}"
            );
            let api_id = created.body["graphqlApi"]["apiId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reported_providers(&created, "graphqlApi").len(),
                count,
                "case {case:?}: expected {count} providers on the create reply, got {:?}",
                reported_providers(&created, "graphqlApi")
            );

            // Every configuration the request named is reported back, distinctly: the
            // identities read off the reply are as many as the request carried.
            let read = get_api(&router, &api_id).await;
            let mut identities: Vec<String> = reported_providers(&read, "graphqlApi")
                .iter()
                .map(|reported| reported_identity(reported, authentication_type))
                .collect();
            identities.sort();
            identities.dedup();
            assert_eq!(
                identities.len(),
                count,
                "case {case:?}: expected {count} distinct configurations read back, got \
                 {identities:?}"
            );

            // The same list through the UPDATE path, which carries the same member.
            let updated = update_api_with(
                &router,
                &api_id,
                &format!("Api_update_{authentication_type}_{count}"),
                &providers,
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?} (update): expected UpdateGraphqlApi 200, got {updated:?}"
            );
            assert_eq!(
                reported_providers(&updated, "graphqlApi").len(),
                count,
                "case {case:?} (update): expected {count} providers back, got {:?}",
                reported_providers(&updated, "graphqlApi")
            );
        }
    }
}

/// Rule 1: a provider that repeats a configuration the API already has is refused, by its
/// 1-based position — whether the repeat is of another provider's configuration or of the
/// API's own authentication mode — and a type set up through no configuration can be named
/// only once.
#[tokio::test]
async fn a_repeated_way_of_authenticating_the_api_is_refused_by_position() {
    let router = router();
    let api_id = crate::error_contract::create_api(&router, "Api_duplicate_providers").await;

    // A configured type repeating one configuration, at a position past the entries that
    // are distinct.
    for (authentication_type, ..) in CONFIGURED_TYPES {
        let providers = vec![
            provider(authentication_type, "first"),
            provider(authentication_type, "second"),
            provider(authentication_type, "first"),
        ];
        let answer = update_api_with(&router, &api_id, "Api_duplicate_providers", &providers).await;
        let expected = format!(
            "Authentication type {authentication_type} for additional authentication provider 3 \
             already specified on the API. It can only be specified once."
        );
        assert_eq!(
            (answer.status, answer.error_type.as_str(), answer.message.as_str()),
            (400, "BadRequestException", expected.as_str()),
            "case a repeated {authentication_type} configuration: expected the duplicate \
             refusal, got {answer:?}"
        );
    }

    // A type set up through no configuration is identified by the type alone, so naming
    // it twice repeats it — and the API's OWN authentication type is one of the ways it is
    // authenticated, so a provider naming that type repeats it at the first position.
    for authentication_type in UNCONFIGURED_TYPES {
        let providers = vec![
            provider(authentication_type, ""),
            provider(authentication_type, ""),
        ];
        let answer = update_api_with(&router, &api_id, "Api_duplicate_providers", &providers).await;
        // The API itself authenticates with `API_KEY` (`create_api`), so a provider of
        // that type repeats the API's own mode and is refused where it stands; a provider
        // of the other type is refused where it repeats the entry before it.
        let position = if *authentication_type == "API_KEY" { 1 } else { 2 };
        let expected = format!(
            "Authentication type {authentication_type} for additional authentication provider \
             {position} already specified on the API. It can only be specified once."
        );
        assert_eq!(
            (answer.status, answer.error_type.as_str(), answer.message.as_str()),
            (400, "BadRequestException", expected.as_str()),
            "case {authentication_type} named twice: expected the duplicate refusal, got \
             {answer:?}"
        );
    }

    // The API's own mode is one of the ways it is authenticated, so a provider naming the
    // same unconfigured type as the API itself repeats it.
    let providers = vec![provider("API_KEY", "")];
    let answer = update_api_with(&router, &api_id, "Api_duplicate_providers", &providers).await;
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, "BadRequestException"),
        "case a provider repeating the API's own API_KEY mode: expected the duplicate refusal, \
         got {answer:?}"
    );
}

/// Rule 2: the quota bounds the list — at the quota the providers are stored, one past it
/// the call is refused with the quota and the submitted count, the stored providers are
/// untouched, and a later update back at the quota succeeds.
#[tokio::test]
async fn the_provider_quota_bounds_the_list_and_a_refusal_changes_nothing() {
    let router = router();
    let api_id = crate::error_contract::create_api(&router, "Api_provider_quota").await;

    // At the quota: stored, and read back in full.
    let at_quota = distinct_providers("OPENID_CONNECT", PROVIDERS_PER_API);
    let accepted = update_api_with(&router, &api_id, "Api_provider_quota", &at_quota).await;
    assert_eq!(
        accepted.status, 200,
        "case {PROVIDERS_PER_API} providers: expected 200, got {accepted:?}"
    );

    // One past the quota, and well past it: refused, naming the quota and the count.
    for count in [PROVIDERS_PER_API + 1, PROVIDERS_PER_API + 13] {
        let providers = distinct_providers("OPENID_CONNECT", count);
        let answer = update_api_with(&router, &api_id, "Api_provider_quota", &providers).await;
        let expected = format!(
            "Maximum number of additional authentication providers is {PROVIDERS_PER_API}. You \
             added {count}."
        );
        assert_eq!(
            (answer.status, answer.error_type.as_str(), answer.message.as_str()),
            (400, "BadRequestException", expected.as_str()),
            "case {count} providers: expected the quota refusal, got {answer:?}"
        );

        // The refused update left the stored providers in place.
        let read = get_api(&router, &api_id).await;
        assert_eq!(
            reported_providers(&read, "graphqlApi").len(),
            PROVIDERS_PER_API,
            "case {count} providers: expected the stored {PROVIDERS_PER_API} to survive the \
             refusal, got {:?}",
            reported_providers(&read, "graphqlApi")
        );
    }

    // And the same shape at the quota still succeeds afterwards.
    let again = update_api_with(&router, &api_id, "Api_provider_quota", &at_quota).await;
    assert_eq!(
        again.status, 200,
        "case {PROVIDERS_PER_API} providers after a refusal: expected 200, got {again:?}"
    );
    assert_eq!(
        reported_providers(&again, "graphqlApi").len(),
        PROVIDERS_PER_API,
        "case {PROVIDERS_PER_API} providers after a refusal: expected them all back, got {:?}",
        reported_providers(&again, "graphqlApi")
    );
}
