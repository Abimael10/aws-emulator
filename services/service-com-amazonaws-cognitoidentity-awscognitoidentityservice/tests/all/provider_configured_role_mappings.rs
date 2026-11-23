// SPDX-License-Identifier: CC0-1.0

//! WHICH identity provider an identity pool's role MAPPING may be keyed by.
//!
//! RULE: `SetIdentityPoolRoles` stores a `RoleMappings` entry only for an identity provider
//! the pool's own FEDERATED-PROVIDER configuration declares — an entry of
//! `SupportedLoginProviders`, a `CognitoIdentityProviders` user pool (named on its own or
//! with an app client id the pool carries for it, the spelling
//! `SetIdentityPoolRolesInput$RoleMappings` documents: "for example, graph.facebook.com or
//! cognito-idp.us-east-1.amazonaws.com/us-east-1_abcdefghi:app_client_id"), a
//! `SamlProviderARNs` or an `OpenIdConnectProviderARNs` provider — because a mapping resolves
//! a role from the CLAIMS of a token that provider issues. A key that configuration does not
//! carry is refused with the modelled `InvalidParameterException` (HTTP 400) and AWS's own
//! sentence over the key the request sent, and nothing of the call is stored.
//!
//! The rows span what the rule generalises over: WHICH configuration member declares the
//! provider (so every provider kind a pool can be fronted by is a row), the FORM of the key
//! (a public provider's host, a user pool's issuer, that issuer with an app client id, an
//! ARN), and — for the refused rows — why the key names no provider of the pool (a provider
//! of no pool, a declared user pool with an app client it does not carry, the pool's own
//! developer provider, a prefix of a declared name, a declared name in another case, an ARN
//! the pool does not hold).
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds.

use serde_json::{json, Value};

use crate::identity_pool_flow::{error_code, member, Emulator};
use crate::identity_pool_roles::role_arn;

/// The Amazon Cognito user pool the fixture pool is fronted by, named the way `GetId.Logins`
/// documents a user pool (`cognito-idp.<region>.amazonaws.com/<UserPoolId>`).
const USER_POOL_PROVIDER: &str = "cognito-idp.us-east-1.amazonaws.com/us-east-1_mapkeys01";

/// The app client of that user pool the fixture pool carries, which a role mapping of that
/// provider names beside the provider name.
const CLIENT_ID: &str = "mapkeysclient1";

/// A public login provider the fixture pool declares, identified by its name alone.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// A SAML provider is declared by the ARN of its IAM registration ("A list of the SAML
/// provider ARNs", this service's model).
const SAML_PROVIDER_ARN: &str = "arn:aws:iam::000000000000:saml-provider/mapkeys-saml";

/// The issuer of the OpenID Connect provider the fixture pool declares: the ARN of the IAM
/// registration carries that issuer, and a login — and a mapping — of it is made under the
/// issuer itself.
const OIDC_ISSUER: &str = "accounts.google.com";

/// The pool's own developer provider — "the 'domain' by which Cognito will refer to your
/// users", this account's own back end rather than a federated provider, so no token of it
/// carries claims a mapping could read.
const DEVELOPER_PROVIDER: &str = "login.mapkeys.developer";

/// How AWS words the refusal, over the key the request filed the mapping under.
fn not_a_mapping_provider(provider: &str) -> String {
    format!("({provider}) is not a valid RoleMapping ProviderName or is not a configured provider.")
}

/// The message an error response carries.
fn message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an error response carries a message: {body}"))
        .to_string()
}

/// A pool fronted by one provider of every federated kind, plus a developer provider.
async fn pool(emulator: &Emulator, name: &str) -> String {
    emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "SupportedLoginProviders": {PUBLIC_PROVIDER: "declared-app-id"},
                "CognitoIdentityProviders": [
                    {"ProviderName": USER_POOL_PROVIDER, "ClientId": CLIENT_ID},
                ],
                "SamlProviderARNs": [SAML_PROVIDER_ARN],
                "OpenIdConnectProviderARNs": [
                    format!("arn:aws:iam::000000000000:oidc-provider/{OIDC_ISSUER}"),
                ],
            }),
        )
        .await["IdentityPoolId"]
        .as_str()
        .expect("CreateIdentityPool answers an IdentityPoolId")
        .to_string()
}

/// The `Roles` map every call below carries: a role of this account, so a refusal is about
/// the mapping's key and never about the roles beside it.
fn roles() -> Value {
    json!({"authenticated": role_arn("Cognito_mapKeysAuth_Role")})
}

/// A complete `Token` mapping keyed by `provider`: the type is the one that reads the
/// token's own role claims, with the `AmbiguousRoleResolution` the model requires for it, so
/// every row below differs only in the KEY.
fn mapping(provider: &str) -> Value {
    json!({provider: {"Type": "Token", "AmbiguousRoleResolution": "AuthenticatedRole"}})
}

/// Set `pool_id`'s roles with one mapping keyed by `provider`, answering the status and body.
async fn set_mapping(emulator: &Emulator, pool_id: &str, provider: &str) -> (u16, Value) {
    emulator
        .call(
            "SetIdentityPoolRoles",
            &json!({
                "IdentityPoolId": pool_id,
                "Roles": roles(),
                "RoleMappings": mapping(provider),
            }),
        )
        .await
}

/// What `GetIdentityPoolRoles` reports for `pool_id`.
async fn reported_roles(emulator: &Emulator, pool_id: &str) -> Value {
    emulator
        .ok("GetIdentityPoolRoles", &json!({"IdentityPoolId": pool_id}))
        .await
}

#[tokio::test]
async fn a_mapping_is_stored_only_for_a_provider_the_pool_declares() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "mapping-keys").await;

    // (case, the provider the mapping is keyed by, declared by the pool?)
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "a SupportedLoginProviders entry",
            PUBLIC_PROVIDER.to_string(),
            true,
        ),
        (
            "a CognitoIdentityProviders user pool",
            USER_POOL_PROVIDER.to_string(),
            true,
        ),
        (
            "that user pool with the app client the pool carries",
            format!("{USER_POOL_PROVIDER}:{CLIENT_ID}"),
            true,
        ),
        (
            "a SamlProviderARNs provider",
            SAML_PROVIDER_ARN.to_string(),
            true,
        ),
        (
            "an OpenIdConnectProviderARNs issuer",
            OIDC_ISSUER.to_string(),
            true,
        ),
        (
            "a public provider the pool was never given",
            "www.amazon.com".to_string(),
            false,
        ),
        (
            "that user pool with an app client the pool does not carry",
            format!("{USER_POOL_PROVIDER}:anotherclient"),
            false,
        ),
        (
            "a user pool of no pool, with an app client",
            "cognito-idp.us-east-1.amazonaws.com/us-east-1_other999:someclient".to_string(),
            false,
        ),
        (
            "the pool's own developer provider",
            DEVELOPER_PROVIDER.to_string(),
            false,
        ),
        (
            "a prefix of the declared user pool's name",
            "cognito-idp.us-east-1.amazonaws.com".to_string(),
            false,
        ),
        (
            "a declared name in another case",
            PUBLIC_PROVIDER.to_uppercase(),
            false,
        ),
        (
            "a SAML provider ARN the pool does not carry",
            "arn:aws:iam::000000000000:saml-provider/other-saml".to_string(),
            false,
        ),
    ];

    for (case, provider, declared) in rows {
        // The configuration a refused call has to leave exactly where it was: a mapping of a
        // provider the pool does declare, stored before the row's own call.
        emulator
            .ok(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": roles(),
                    "RoleMappings": mapping(PUBLIC_PROVIDER),
                }),
            )
            .await;

        let (status, body) = set_mapping(&emulator, &pool_id, &provider).await;
        let got = reported_roles(&emulator, &pool_id).await;
        if declared {
            assert_eq!(
                status, 200,
                "case {case:?}: a mapping for a declared provider is stored, got {status} with \
                 {body}"
            );
            // Read back through the implemented reader: what was stored is what the get
            // reports, and the whole configuration is this call's.
            assert_eq!(
                member(&got, "RoleMappings"),
                Some(mapping(&provider)),
                "case {case:?}: the read reports the mapping that was stored, got {got}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: expected (400, InvalidParameterException), got ({status}, {body})"
        );
        assert_eq!(
            message(case, &body),
            not_a_mapping_provider(&provider),
            "case {case:?}: expected {:?}, got {body}",
            not_a_mapping_provider(&provider)
        );
        // The refused call stored nothing: the pool still reports the configuration it had.
        assert_eq!(
            member(&got, "RoleMappings"),
            Some(mapping(PUBLIC_PROVIDER)),
            "case {case:?}: a refused call leaves the stored RoleMappings alone, got {got}"
        );
    }
}

/// RULE, applied to the configuration a pool ends up with: the providers a pool DECLARES are
/// whatever its latest configuration states, so a mapping follows the configuration — a
/// provider an update adds becomes mappable, and one an update drops stops being.
#[tokio::test]
async fn the_mappable_set_follows_the_pools_current_configuration() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "mapping-keys-updated").await;
    let added = "appleid.apple.com";

    // Before the update the pool does not carry it, so no mapping can be keyed by it.
    let (status, body) = set_mapping(&emulator, &pool_id, added).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterException"),
        "case \"before the update\": expected the provider to be refused, got ({status}, {body})"
    );

    // The update REPLACES the pool's configuration with one carrying the added provider and
    // no longer carrying the public provider it was created with.
    emulator
        .ok(
            "UpdateIdentityPool",
            &json!({
                "IdentityPoolId": &pool_id,
                "IdentityPoolName": "mapping-keys-updated",
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "SupportedLoginProviders": {added: "added-app-id"},
            }),
        )
        .await;

    let (status, body) = set_mapping(&emulator, &pool_id, added).await;
    assert_eq!(
        status, 200,
        "case \"after the update\": a provider the configuration now declares is mappable, got \
         {status} with {body}"
    );
    for (case, provider) in [
        ("a provider the update dropped", PUBLIC_PROVIDER.to_string()),
        (
            "a user pool the update dropped",
            format!("{USER_POOL_PROVIDER}:{CLIENT_ID}"),
        ),
    ] {
        let (status, body) = set_mapping(&emulator, &pool_id, &provider).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: expected it to be refused, got ({status}, {body})"
        );
        assert_eq!(
            message(case, &body),
            not_a_mapping_provider(&provider),
            "case {case:?}: expected AWS's sentence over the key, got {body}"
        );
    }
}
