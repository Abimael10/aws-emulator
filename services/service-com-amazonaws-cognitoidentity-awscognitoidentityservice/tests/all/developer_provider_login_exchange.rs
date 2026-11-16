// SPDX-License-Identifier: CC0-1.0

//! What a `Logins` entry keyed by the identity pool's OWN developer provider buys at the
//! credential exchange.
//!
//! RULE: `GetCredentialsForIdentity` redeems a presented login only as a TOKEN its provider
//! issued — "Any provided logins will be validated against supported login providers. If the
//! token is for cognito-identity.amazonaws.com, it will be passed through to Security Token
//! Service with the appropriate role for the token"
//! (`smithy_query(query="doc",shape="GetCredentialsForIdentityInput")`) — and the pool's own
//! `DeveloperProviderName` is not one of its login providers but "the 'domain' by which
//! Cognito will refer to your users … a placeholder that allows your backend and the Cognito
//! service to communicate about the developer provider"
//! (`smithy_query(query="doc",shape="CreateIdentityPoolInput",member="DeveloperProviderName")`),
//! which issues no token at all. So an entry keyed by that name carries a developer user
//! IDENTIFIER where a token has to be, and the exchange is refused with the modelled
//! `NotAuthorizedException` carrying AWS's fixed sentence `Invalid login token. Not a Cognito
//! token.` and vends nothing — whichever identity the pool has registered that identifier
//! under, and whatever else the request asks for: like every other presented-login refusal,
//! it is decided BEFORE the request's role-selection parameters (`CustomRoleArn`) and before
//! the pool's own role configuration.
//!
//! A developer-authenticated identity reaches this exchange the documented way instead: with
//! the `Token` its `GetOpenIdTokenForDeveloperIdentity` mint handed back, presented under this
//! service's own provider name — which is the control every table below ends with.
//!
//! The rows span the class the rule generalises over: the identifier presented (the one this
//! identity is registered under, one registered under ANOTHER identity of the pool, one no
//! mint ever registered, a single-character one, and a value that IS this identity's own
//! token), the pool's own `DeveloperProviderName` (modelled `length 1..128, pattern
//! ^[\w._-]+$`, `smithy_query(query="shape",shape="DeveloperProviderName")` — its shortest and
//! longest spellings and the separators the pattern allows), the SHAPE of the map (that entry
//! alone, beside a public-provider login, beside the identity's own minted Cognito token),
//! what else the request asks for (nothing, a `CustomRoleArn` the pool's configuration does
//! not permit) and the pool's role configuration (both roles configured, none at all).
//!
//! The second table also keeps the `CustomRoleArn` parameter refusal in view: a request whose
//! login stands up — the identity's own live token, and a public-provider login — still draws
//! that refusal, so the rows above are known to have been answered for their LOGIN and not for
//! the parameter.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds, with the role a vend assumes seeded into
//! the IAM store of that same engine.

use serde_json::{json, Map, Value};

use crate::developer_identity_tokens::token_request;
use crate::identity_pool_flow::{cognito_trust_policy, error_code, seeded_role_arn, Emulator};

/// The operation the rule is asserted at.
const OPERATION: &str = "GetCredentialsForIdentity";

/// The `Logins` provider name a token minted by THIS service is presented under. Spelled as a
/// literal so the wire-visible name is pinned by the test rather than by the code under test.
const COGNITO_IDENTITY_PROVIDER: &str = "cognito-identity.amazonaws.com";

/// A public login provider, for the rows whose map carries a second entry beside the one under
/// test: one of the names `GetId$Logins` publishes, whose login is an opaque OAuth access
/// token.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// The opaque value a login of that provider carries.
const PUBLIC_LOGIN: &str = "an-oauth-access-token";

/// The status every refusal of this service travels with, whatever `@httpError` the shape it
/// answers is annotated with (`crate::service` owns that rule).
const REFUSED_STATUS: u16 = 400;

/// AWS's fixed sentences, spelled as literals so the wire-visible text is pinned by the test.
/// Each is fixed text: no part of it is taken from the request or from stored state.
const NOT_A_COGNITO_TOKEN: &str = "Invalid login token. Not a Cognito token.";
const CUSTOM_ROLE_ARN_UNSUPPORTED: &str =
    "Only SAML providers and providers with RoleMappings support custom role ARN.";

/// The modelled bounds of `DeveloperProviderName` (`length 1..128, pattern ^[\w._-]+$`,
/// `smithy_query(query="shape",shape="DeveloperProviderName")`): the shortest and the longest
/// spelling a pool can carry.
const MIN_DEVELOPER_PROVIDER_LEN: usize = 1;
const MAX_DEVELOPER_PROVIDER_LEN: usize = 128;

/// The developer provider spellings the tables run over: the modelled minimum, the modelled
/// maximum, and the separators the pattern allows — none of them the one the issue cited.
fn developer_provider_spellings() -> Vec<(&'static str, String)> {
    vec![
        (
            "the modelled minimum length",
            "d".repeat(MIN_DEVELOPER_PROVIDER_LEN),
        ),
        ("a dotted domain", "login.example.backend".to_string()),
        ("underscores and dashes", "my_dev-provider_1".to_string()),
        (
            "the modelled maximum length",
            format!("{}.app", "d".repeat(MAX_DEVELOPER_PROVIDER_LEN - 4)),
        ),
    ]
}

/// A pool that names `developer_provider` and the public provider the rows present a second
/// login of, and — when `with_roles` — the two roles a vend assumes, seeded into the IAM store
/// of the same engine. Answers its `IdentityPoolId`.
async fn vending_pool(
    emulator: &Emulator,
    name: &str,
    developer_provider: &str,
    with_roles: bool,
) -> String {
    let created = emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": developer_provider,
                "SupportedLoginProviders": {PUBLIC_PROVIDER: "0123456789"},
            }),
        )
        .await;
    let pool_id = created["IdentityPoolId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateIdentityPool answers an IdentityPoolId: {created}"))
        .to_string();
    if with_roles {
        let authenticated =
            emulator.seed_role(&seeded_role_arn(name, "auth"), cognito_trust_policy(&pool_id));
        let unauthenticated = emulator.seed_role(
            &seeded_role_arn(name, "unauth"),
            cognito_trust_policy(&pool_id),
        );
        emulator
            .ok(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": {
                        "authenticated": authenticated,
                        "unauthenticated": unauthenticated,
                    },
                }),
            )
            .await;
    }
    pool_id
}

/// Register `identifier` as a developer user of `pool_id` under `developer_provider`, and
/// answer the identity it names together with the OpenID token the mint handed back.
async fn register(
    emulator: &Emulator,
    pool_id: &str,
    developer_provider: &str,
    identifier: &str,
) -> (String, String) {
    let minted = emulator
        .ok(
            "GetOpenIdTokenForDeveloperIdentity",
            &token_request(pool_id, &[(developer_provider, identifier)], None, None),
        )
        .await;
    let identity_id = minted["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("the mint answers an IdentityId: {minted}"))
        .to_string();
    let token = minted["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("the mint answers a Token: {minted}"))
        .to_string();
    (identity_id, token)
}

/// The exchange itself: `GetCredentialsForIdentity` for `identity_id`, presenting `logins` and
/// the `CustomRoleArn` the row names (none when it names none).
async fn exchange(
    emulator: &Emulator,
    identity_id: &str,
    logins: &Value,
    custom_role_arn: Option<&str>,
) -> (u16, Value) {
    let mut request = Map::new();
    request.insert("IdentityId".to_string(), json!(identity_id));
    request.insert("Logins".to_string(), logins.clone());
    if let Some(custom_role_arn) = custom_role_arn {
        request.insert("CustomRoleArn".to_string(), json!(custom_role_arn));
    }
    emulator.call(OPERATION, &Value::Object(request)).await
}

/// Assert that the exchange was refused with `code` and the fixed sentence `message`, and that
/// it vended nothing.
fn assert_refused(case: &str, status: u16, body: &Value, code: &str, message: &str) {
    assert_eq!(
        status, REFUSED_STATUS,
        "case {case:?}: expected HTTP {REFUSED_STATUS} for a refused exchange, got {status} \
         with {body}"
    );
    let answered = error_code(body);
    assert!(
        answered.ends_with(code),
        "case {case:?}: expected {code}, got {answered:?} in {body}"
    );
    let actual = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        actual, message,
        "case {case:?}: expected the fixed sentence {message:?}, got {actual:?} in {body}"
    );
    assert!(
        body.get("Credentials").is_none(),
        "case {case:?}: a refused exchange vends nothing, got {body}"
    );
}

/// Assert that the exchange was SERVED and carries a credential.
fn assert_served(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected the exchange to be served, got {status} with {body}"
    );
    assert!(
        body["Credentials"]["AccessKeyId"].is_string(),
        "case {case:?}: a served exchange carries Credentials.AccessKeyId, got {body}"
    );
}

/// RULE: an entry keyed by the pool's own developer provider carries no token, so the exchange
/// is refused as no Cognito token — whichever identifier it carries, whichever developer
/// provider spelling the pool uses and whatever else the map carries.
#[tokio::test]
async fn a_developer_provider_entry_buys_no_credential_at_the_exchange() {
    let emulator = Emulator::new();

    for (spelling, developer_provider) in developer_provider_spellings() {
        let pool_name = format!("dev-login-exchange-{}", developer_provider.len());
        let pool_id = vending_pool(&emulator, &pool_name, &developer_provider, true).await;
        let (identity_id, own_token) =
            register(&emulator, &pool_id, &developer_provider, "backend-user-1").await;
        let (sibling_identity, _) =
            register(&emulator, &pool_id, &developer_provider, "backend-user-2").await;
        assert_ne!(
            sibling_identity, identity_id,
            "case {spelling:?}: the two developer users must be two identities"
        );

        // (case, the value presented under the pool's developer provider name)
        let identifiers: Vec<(&str, &str)> = vec![
            (
                "the identifier this identity is registered under",
                "backend-user-1",
            ),
            (
                "an identifier registered under another identity",
                "backend-user-2",
            ),
            (
                "an identifier no mint ever registered",
                "never-registered-user",
            ),
            ("a single-character identifier", "u"),
            // A value that IS a Cognito token of this identity, presented under the wrong
            // key: the key decides, not the value.
            (
                "this identity's own token under the developer provider key",
                own_token.as_str(),
            ),
        ];

        // ...each presented alone, beside a public-provider login, and beside the identity's
        // own minted token, so the rule holds over the SHAPE of the map as well.
        for (identifier_case, presented) in &identifiers {
            for (shape, logins) in [
                ("alone", json!({&developer_provider: presented})),
                (
                    "beside a public-provider login",
                    json!({
                        &developer_provider: presented,
                        PUBLIC_PROVIDER: "an-oauth-access-token",
                    }),
                ),
                (
                    "beside this identity's own minted token",
                    json!({
                        &developer_provider: presented,
                        COGNITO_IDENTITY_PROVIDER: &own_token,
                    }),
                ),
            ] {
                let case = format!("{spelling}, {identifier_case}, {shape}");
                let (status, body) = exchange(&emulator, &identity_id, &logins, None).await;
                assert_refused(
                    &case,
                    status,
                    &body,
                    "NotAuthorizedException",
                    NOT_A_COGNITO_TOKEN,
                );
            }
        }

        // The control: the SAME identity of the SAME pool, presenting the token its own mint
        // handed back under this service's provider name, is served — so every row above was
        // refused for the login it presented and not for the identity, the pool or its roles.
        let (status, body) = exchange(
            &emulator,
            &identity_id,
            &json!({COGNITO_IDENTITY_PROVIDER: &own_token}),
            None,
        )
        .await;
        assert_served(
            &format!("{spelling}, the documented exchange"),
            status,
            &body,
        );
    }
}

/// RULE: the presented login is decided BEFORE the request's role-selection parameters, so a
/// developer-provider entry is answered for the login — not with the `CustomRoleArn` parameter
/// refusal and not with the pool-configuration one — while a request whose login stands up
/// still meets both of those rules.
#[tokio::test]
async fn the_developer_provider_entry_is_decided_before_the_role_the_vend_would_assume() {
    let emulator = Emulator::new();
    let developer_provider = "login.example.ordering";
    let configured = vending_pool(&emulator, "dev-login-ordering", developer_provider, true).await;
    // A pool in the same shape but with NO role configuration at all: there the exchange has no
    // role to resolve, which is the other refusal a vend answers after the login.
    let unconfigured =
        vending_pool(&emulator, "dev-login-ordering-bare", developer_provider, false).await;

    let (identity_id, own_token) =
        register(&emulator, &configured, developer_provider, "ordering-user").await;
    let (bare_identity, bare_token) =
        register(&emulator, &unconfigured, developer_provider, "ordering-user").await;

    // A role ARN the pool's configuration does not permit the exchange to name: neither pool
    // declares a SAML provider or holds a `RoleMappings` entry, so naming a custom role at all
    // is the parameter refusal for an exchange that presents an identity-provider login.
    let custom_role_arn = seeded_role_arn("dev-login-ordering", "custom");

    // An identity of the same pool reached through the PUBLIC provider, for the contrast row:
    // that exchange presents an identity-provider login, which is what the parameter refusal
    // is about.
    let public_identity = emulator
        .identity(&configured, Some(json!({PUBLIC_PROVIDER: PUBLIC_LOGIN})))
        .await;

    // (case, the identity, the logins presented, the CustomRoleArn, the code owed, the sentence
    // owed)
    type Row<'a> = (&'a str, &'a str, Value, Option<&'a str>, &'a str, &'a str);
    let rows: Vec<Row<'_>> = vec![
        (
            "a developer-provider entry beside a CustomRoleArn the pool does not permit",
            identity_id.as_str(),
            json!({developer_provider: "ordering-user"}),
            Some(custom_role_arn.as_str()),
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        ),
        (
            "a developer-provider entry against a pool that configures no role",
            bare_identity.as_str(),
            json!({developer_provider: "ordering-user"}),
            None,
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        ),
        (
            "a developer-provider entry against that pool, beside the same CustomRoleArn",
            bare_identity.as_str(),
            json!({developer_provider: "ordering-user"}),
            Some(custom_role_arn.as_str()),
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        ),
        (
            "the same identifier under this service's own provider key, beside that \
             CustomRoleArn",
            identity_id.as_str(),
            json!({COGNITO_IDENTITY_PROVIDER: "ordering-user"}),
            Some(custom_role_arn.as_str()),
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        ),
        (
            "the identity's own live token beside that same CustomRoleArn",
            identity_id.as_str(),
            json!({COGNITO_IDENTITY_PROVIDER: &own_token}),
            Some(custom_role_arn.as_str()),
            "InvalidParameterException",
            CUSTOM_ROLE_ARN_UNSUPPORTED,
        ),
        (
            // The same parameter refusal for an identity-provider login, of a provider that
            // is neither SAML nor one the pool maps roles for.
            "a public-provider login beside that same CustomRoleArn",
            public_identity.as_str(),
            json!({PUBLIC_PROVIDER: PUBLIC_LOGIN}),
            Some(custom_role_arn.as_str()),
            "InvalidParameterException",
            CUSTOM_ROLE_ARN_UNSUPPORTED,
        ),
    ];

    for (case, identity, logins, custom_role_arn, code, owed) in &rows {
        let (status, body) = exchange(&emulator, identity, logins, *custom_role_arn).await;
        assert_refused(case, status, &body, code, owed);
    }

    // The control for the unconfigured pool: the same identity presenting a login that DOES
    // stand up reaches that pool's own configuration, which is how the rows above are known to
    // have been answered for their login rather than for the pool.
    let (status, body) = exchange(
        &emulator,
        &bare_identity,
        &json!({COGNITO_IDENTITY_PROVIDER: &bare_token}),
        None,
    )
    .await;
    assert_eq!(
        status, REFUSED_STATUS,
        "a pool that configures no role has none to assume, got {status} with {body}"
    );
    let answered = error_code(&body);
    assert!(
        answered.ends_with("InvalidIdentityPoolConfigurationException"),
        "a login that stands up reaches the pool's own configuration, got {answered:?} in {body}"
    );
}

/// RULE: the refusal changes nothing — the developer registration it names is still the
/// identity's, so the documented exchange still serves it afterwards, and the developer
/// identity operations that TAKE such an identifier are not refused by this rule.
#[tokio::test]
async fn a_refused_developer_provider_entry_leaves_the_registration_alone() {
    let emulator = Emulator::new();
    let developer_provider = "login.example.retained";
    let pool_id = vending_pool(&emulator, "dev-login-retained", developer_provider, true).await;
    let (identity_id, _) = register(&emulator, &pool_id, developer_provider, "retained-user").await;

    for attempt in 0..3 {
        let (status, body) = exchange(
            &emulator,
            &identity_id,
            &json!({developer_provider: "retained-user"}),
            None,
        )
        .await;
        assert_refused(
            &format!("attempt {attempt}"),
            status,
            &body,
            "NotAuthorizedException",
            NOT_A_COGNITO_TOKEN,
        );
    }

    // The mint that TAKES the developer identifier still resolves the same identity...
    let (again, token) = register(&emulator, &pool_id, developer_provider, "retained-user").await;
    assert_eq!(
        again, identity_id,
        "the registration survives the refusals: expected {identity_id}, got {again}"
    );
    // ...and the token it hands back still buys the credential.
    let (status, body) = exchange(
        &emulator,
        &identity_id,
        &json!({COGNITO_IDENTITY_PROVIDER: &token}),
        None,
    )
    .await;
    assert_served("the exchange after the refusals", status, &body);
}
