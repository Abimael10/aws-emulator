// SPDX-License-Identifier: CC0-1.0

//! What `AssumeRoleWithWebIdentity` reports the exchanged token's PROVIDER as.
//!
//! RULE: the issuing authority a web identity exchange reports is the identifier the
//! identity provider is known by — the token's issuer with its URL scheme dropped,
//! `<host>[:<port>]/<path>` — which is the form an OpenID Connect provider is registered
//! and keyed under, the form the trailing segment of its ARN carries, the form a trust
//! policy names it by as a `Federated` principal and the form its condition keys are
//! prefixed with. A scheme is never part of it, so a client that correlates the exchange
//! with the provider it registered compares the two values directly.
//!
//! Every row drives the real path end to end on ONE shared engine, through the owning
//! services' own operations: the pool, its app client and its user are created in Amazon
//! Cognito, the ID token comes from a real sign-in, the provider is registered through
//! `iam:CreateOpenIDConnectProvider` and the role's trust policy is the document
//! `iam:CreateRole` stored. The rows differ along what the rule generalises over — which
//! issuer the token names, in each part the identifier is built from: the HOST (a pool
//! created through this emulator's own address publishes an issuer on it, one created
//! through the service's regional endpoint publishes the documented host), the PORT that
//! address carries, and the PATH, which is the pool's own id.
//!
//! The issuer that carries NO scheme to drop — `cognito-identity.amazonaws.com`, which
//! every identity pool's own tokens name — is reported unchanged by the same derivation;
//! that leg needs the service that mints such a token, so it lives in
//! `core/router/tests/all/sts_web_identity_federation.rs`.

use smithy_server_runtime::MultiServiceRouter;

use super::sts_sessions::{
    element, emulator_account, member, query, STS_API_VERSION, STS_SIGNING_NAME,
};
use super::web_identity_verification::{
    create_federated_role, emulator_with_providers, exchange, register_provider, sign_in,
    sign_in_at, SignedIn,
};

/// The regional endpoint of the user-pool service, the host a client that resolves AWS's
/// own hostnames reaches a pool at — and the host a pool created through it publishes its
/// issuer on.
const REGIONAL_COGNITO_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// This emulator's own address, which names no service: a pool created through it publishes
/// its token issuer there, so its issuer carries a PORT — the shape an issuer has for every
/// client pointed at a locally served emulator.
const EMULATOR_AUTHORITY: &str = "127.0.0.1:9297";

/// The same emulator bound on another port, so nothing is keyed to one of them.
const OTHER_EMULATOR_AUTHORITY: &str = "127.0.0.1:18443";

/// The scheme an OpenID Connect issuer URL carries, and the one thing the provider
/// identifier never has: "The URL must begin with https://"
/// (`iam:CreateOpenIDConnectProvider$Url`).
const ISSUER_SCHEME: &str = "https://";

/// The identifier `issuer` names its provider by, derived here rather than read out of the
/// service: the issuer with its scheme dropped and no trailing separator.
fn provider_identifier(issuer: &str) -> String {
    issuer
        .strip_prefix(ISSUER_SCHEME)
        .unwrap_or(issuer)
        .trim_end_matches('/')
        .to_string()
}

/// How this account's OpenID Connect provider ARNs are spelled:
/// `arn:aws:iam::<account>:oidc-provider/<identifier>`.
fn provider_arn_of(identifier: &str) -> String {
    format!(
        "arn:aws:iam::{}:oidc-provider/{identifier}",
        emulator_account()
    )
}

/// One row's setup: a pool signed in, its issuer registered, and a role whose trust policy
/// admits that provider for the token's own audience.
struct Registered {
    signed_in: SignedIn,
    role_arn: String,
    /// The ARN `iam:CreateOpenIDConnectProvider` answered for the issuer it registered.
    provider_arn: String,
}

/// Sign in to a pool of its own, created through the endpoint `host` — which is the
/// authority its token issuer names — register that issuer as an OpenID Connect provider of
/// the account, and create a role admitting that provider for the token's audience.
async fn register(router: &MultiServiceRouter, name: &str, host: &str) -> Registered {
    let signed_in = sign_in_at(router, name, host).await;
    let provider_arn = register_provider(router, &signed_in.issuer, &signed_in.client_id).await;
    let role_arn = create_federated_role(
        router,
        &format!("{name}-provider-identifier"),
        &provider_arn,
        // The condition keys the clause is written over are prefixed with the provider
        // identifier, which is the very value the exchange reports: a role admitted under
        // this clause is a role whose policy and whose response agree on one spelling.
        &provider_identifier(&signed_in.issuer),
        &signed_in.client_id,
    )
    .await;
    Registered {
        signed_in,
        role_arn,
        provider_arn,
    }
}

/// RULE: the exchange reports the provider identifier — the issuer without its scheme —
/// whichever issuer the token names.
#[tokio::test]
async fn the_exchange_reports_the_issuer_without_its_scheme() {
    let (router, _engine) = emulator_with_providers();

    // One row: the case, the pool, and the endpoint the pool is created through — which is
    // the authority its token issuer names, and so the part of the identifier before the
    // pool's own id.
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a pool at the service's regional endpoint",
            "provider-regional",
            REGIONAL_COGNITO_HOST,
        ),
        (
            // A second pool at the same endpoint: a different path, so nothing is keyed to
            // one issuer.
            "another pool at the same endpoint",
            "provider-regional-second",
            REGIONAL_COGNITO_HOST,
        ),
        (
            // The shape a client pointed at a locally served emulator sees: the authority
            // carries a port, which is part of the identifier.
            "a pool at this emulator's own address, which carries a port",
            "provider-loopback",
            EMULATOR_AUTHORITY,
        ),
        (
            "a pool at the same address on another port",
            "provider-loopback-other-port",
            OTHER_EMULATOR_AUTHORITY,
        ),
    ];

    for (case, pool_name, host) in rows {
        let registered = register(&router, pool_name, host).await;
        let issuer = registered.signed_in.issuer.clone();
        let expected = provider_identifier(&issuer);
        assert!(
            issuer.starts_with(ISSUER_SCHEME) && issuer.contains(host),
            "case {case:?}: expected the pool to publish an issuer at {host:?}, got \
             {issuer:?}"
        );
        let (status, body) = exchange(
            &router,
            &registered.role_arn,
            &registered.signed_in.id_token,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the exchange to be served, got {status} {body}"
        );
        let reported = member(case, &body, "Provider");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the provider identifier {expected:?} for the issuer \
             {issuer:?}, got {reported:?}"
        );
        assert!(
            !reported.contains("://"),
            "case {case:?}: the provider identifier carries no URL scheme, got {reported:?}"
        );
        assert!(
            !reported.ends_with('/'),
            "case {case:?}: the provider identifier ends in no separator, got {reported:?}"
        );
        // The correlation the rule is FOR: the value the exchange reports is the one the
        // account's provider is keyed under, so the ARN a client builds from it is the ARN
        // IAM answered when it registered the provider.
        let built = provider_arn_of(&reported);
        assert_eq!(
            built, registered.provider_arn,
            "case {case:?}: expected the reported identifier to build the registered \
             provider ARN {:?}, got {built:?}",
            registered.provider_arn
        );
        // The session is real, whatever the response reports about the provider: the
        // credential it vended resolves to the assumed-role identity when it is read back.
        let access_key_id = member(case, &body, "AccessKeyId");
        let assumed_role_arn = member(case, &body, "Arn");
        let (status, read) = query(
            &router,
            &access_key_id,
            STS_SIGNING_NAME,
            STS_API_VERSION,
            "GetCallerIdentity",
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the vended credential must resolve, got {status} {read}"
        );
        assert_eq!(
            element(&read, "Arn").as_deref(),
            Some(assumed_role_arn.as_str()),
            "case {case:?}: expected the session to report {assumed_role_arn}, got {read}"
        );
    }
}

/// RULE, the policy side: the identifier the exchange reports is the one a trust policy's
/// condition keys are prefixed with, so a clause written over the REPORTED spelling decides
/// the token and a clause written over the raw issuer URL decides nothing and withholds the
/// grant.
#[tokio::test]
async fn a_clause_is_decided_only_under_the_reported_provider_identifier() {
    let (router, _engine) = emulator_with_providers();
    let signed_in = sign_in(&router, "provider-condition").await;
    let provider_arn = register_provider(&router, &signed_in.issuer, &signed_in.client_id).await;
    let identifier = provider_identifier(&signed_in.issuer);

    // One row: the case, the prefix the audience clause names, and whether a role carrying
    // that clause admits this token.
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "the identifier the exchange reports",
            identifier.clone(),
            true,
        ),
        (
            "the raw issuer URL, scheme included",
            signed_in.issuer.clone(),
            false,
        ),
        (
            "the identifier of another issuer",
            format!("{identifier}.not-this-provider"),
            false,
        ),
    ];

    for (index, (case, prefix, admits)) in rows.into_iter().enumerate() {
        let role_arn = create_federated_role(
            &router,
            &format!("provider-condition-{index}"),
            &provider_arn,
            &prefix,
            &signed_in.client_id,
        )
        .await;
        let (status, body) = exchange(&router, &role_arn, &signed_in.id_token).await;
        if admits {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the exchange to be served, got {status} {body}"
            );
            let reported = member(case, &body, "Provider");
            assert_eq!(
                reported, prefix,
                "case {case:?}: expected the reported identifier to be the clause's own \
                 prefix {prefix:?}, got {reported:?}"
            );
        } else {
            assert_eq!(
                status, 403,
                "case {case:?}: a clause that decides nothing must withhold the grant, got \
                 {status} {body}"
            );
            assert!(
                element(&body, "Credentials").is_none(),
                "case {case:?}: a refused exchange must vend no credential, got {body}"
            );
        }
    }
}

/// The three facts the exchange reports about the token are reported TOGETHER and each from
/// the token itself: the provider identifier, the audience the token was issued to and the
/// subject it names — so a response carries no raw issuer URL anywhere.
#[tokio::test]
async fn the_reported_facts_are_the_tokens_own() {
    let (router, _engine) = emulator_with_providers();
    let signed_in = sign_in(&router, "provider-facts").await;
    let provider_arn = register_provider(&router, &signed_in.issuer, &signed_in.client_id).await;
    let identifier = provider_identifier(&signed_in.issuer);
    let role_arn = create_federated_role(
        &router,
        "provider-facts-role",
        &provider_arn,
        &identifier,
        &signed_in.client_id,
    )
    .await;
    let (status, body) = exchange(&router, &role_arn, &signed_in.id_token).await;
    assert_eq!(
        status, 200,
        "case the reported facts: expected the exchange to be served, got {status} {body}"
    );

    // (the member, the value the token itself carries)
    let reported: Vec<(&str, String)> = vec![
        ("Provider", identifier.clone()),
        ("Audience", signed_in.client_id.clone()),
        ("SubjectFromWebIdentityToken", signed_in.sub.clone()),
    ];
    for (name, expected) in reported {
        let actual = member(name, &body, name);
        assert_eq!(
            actual, expected,
            "case {name:?}: expected {expected:?}, got {actual:?}"
        );
    }
    assert!(
        !body.contains(&signed_in.issuer),
        "case the reported facts: the raw issuer URL {:?} is no part of the response, got \
         {body}",
        signed_in.issuer
    );
    // The provider the account registered is still named by its ARN, which the identifier
    // builds: the exchange and the registration agree on one spelling.
    assert_eq!(
        provider_arn_of(&identifier),
        provider_arn,
        "case the reported facts: expected the identifier to build {provider_arn:?}"
    );
}
