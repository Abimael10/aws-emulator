// SPDX-License-Identifier: CC0-1.0

//! The SIGNATURE on the OpenID Connect token this service vends.
//!
//! RULE: a token Amazon Cognito Identity vends is a signed JWS — `GetOpenIdToken` and
//! `GetOpenIdTokenForDeveloperIdentity` each answer one — so its header names the algorithm
//! it was signed under and the key (`kid`) a verifier finds in the key set the issuer
//! publishes, its third segment carries a signature, and that signature checks out over the
//! token's own encoded header and payload (RFC 7515 §5.2) — the check every relying party of
//! such a token makes, `sts:AssumeRoleWithWebIdentity` and any JOSE library with signature
//! verification on. A value whose header names the unsecured algorithm of RFC 7519 §6, or
//! whose signature segment is empty, is one no issuer signed and no verifier accepts.
//!
//! The rows span what the rule generalises over: WHICH operation minted the token, the flow
//! the mint was in (no login, a federated login, a backend-authenticated developer user),
//! and the validity window asked for — the signature is over whatever claim set those
//! produce. Two further cases pin what a signature IS: a token whose payload is rewritten
//! after it was signed no longer verifies, and the published key set carries only public key
//! parameters.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds.

use serde_json::{json, Map, Value};
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::state::token_signing;

use crate::developer_identity_tokens::SIGNING_ALGORITHM;
use crate::identity_pool_flow::Emulator;

/// The developer provider the pool below is created with, so the developer-authenticated
/// mint has one to name.
const DEVELOPER_PROVIDER: &str = "login.signature.developer";

/// The public login provider the pool declares, so a federated row presents a login of a
/// provider the pool is fronted by.
const PUBLIC_PROVIDER: &str = "graph.facebook.com";

/// Which operation a row mints its token with.
#[derive(Clone, Copy)]
enum Mint {
    /// `GetOpenIdToken`, for an identity the pool already holds.
    ForIdentity,
    /// `GetOpenIdTokenForDeveloperIdentity`, for a user the caller's own back end
    /// authenticated.
    ForDeveloperIdentity,
}

/// A pool carrying a developer provider and fronted by the public provider, answering its
/// minted `IdentityPoolId`.
async fn pool(emulator: &Emulator, name: &str) -> String {
    emulator
        .ok(
            "CreateIdentityPool",
            &json!({
                "IdentityPoolName": name,
                "AllowUnauthenticatedIdentities": true,
                "DeveloperProviderName": DEVELOPER_PROVIDER,
                "SupportedLoginProviders": {PUBLIC_PROVIDER: "declared-app-id"},
            }),
        )
        .await["IdentityPoolId"]
        .as_str()
        .expect("CreateIdentityPool answers an IdentityPoolId")
        .to_string()
}

/// Mint one token and answer it with the identity it was minted for.
async fn minted(
    emulator: &Emulator,
    case: &str,
    pool_id: &str,
    mint: Mint,
    login: Option<(&str, &str)>,
    token_duration: Option<i64>,
) -> (String, String) {
    let answered = match mint {
        Mint::ForIdentity => {
            let logins = login.map(|(provider, token)| {
                let mut entries = Map::new();
                entries.insert(provider.to_string(), json!(token));
                Value::Object(entries)
            });
            let identity_id = emulator.identity(pool_id, logins.clone()).await;
            let mut request = json!({"IdentityId": &identity_id});
            if let Some(logins) = logins {
                request["Logins"] = logins;
            }
            emulator.ok("GetOpenIdToken", &request).await
        }
        Mint::ForDeveloperIdentity => {
            let (provider, identifier) =
                login.expect("a developer mint presents a developer user identifier");
            let mut request = json!({
                "IdentityPoolId": pool_id,
                "Logins": {provider: identifier},
            });
            if let Some(token_duration) = token_duration {
                request["TokenDuration"] = json!(token_duration);
            }
            emulator
                .ok("GetOpenIdTokenForDeveloperIdentity", &request)
                .await
        }
    };
    let identity_id = answered["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an IdentityId is answered: {answered}"))
        .to_string();
    let token = answered["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a Token is answered: {answered}"))
        .to_string();
    (identity_id, token)
}

/// One row of the table: the case, the operation that mints, the login presented (a
/// federated login, or the developer provider with a developer user identifier) and the
/// validity window asked for.
type Row<'a> = (&'a str, Mint, Option<(&'a str, &'a str)>, Option<i64>);

#[tokio::test]
async fn every_vended_token_is_a_jws_the_issuers_published_key_verifies() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "token-signature").await;
    let published = token_signing::token_key_set();

    // (case, the operation that mints, the login presented, the window asked for)
    let rows: Vec<Row<'_>> = vec![
        (
            "GetOpenIdToken for a guest identity",
            Mint::ForIdentity,
            None,
            None,
        ),
        (
            "GetOpenIdToken for a federated identity",
            Mint::ForIdentity,
            Some((PUBLIC_PROVIDER, "a-facebook-access-token")),
            None,
        ),
        (
            "GetOpenIdTokenForDeveloperIdentity with no TokenDuration",
            Mint::ForDeveloperIdentity,
            Some((DEVELOPER_PROVIDER, "backend-user-1")),
            None,
        ),
        (
            "GetOpenIdTokenForDeveloperIdentity at the shortest window",
            Mint::ForDeveloperIdentity,
            Some((DEVELOPER_PROVIDER, "backend-user-2")),
            Some(1),
        ),
        (
            "GetOpenIdTokenForDeveloperIdentity at the longest window",
            Mint::ForDeveloperIdentity,
            Some((DEVELOPER_PROVIDER, "backend-user-3")),
            Some(86_400),
        ),
    ];

    for (case, mint, login, token_duration) in rows {
        let (identity_id, token) =
            minted(&emulator, case, &pool_id, mint, login, token_duration).await;

        let segments: Vec<&str> = token.split('.').collect();
        assert_eq!(
            segments.len(),
            3,
            "case {case:?}: a compact JWS has three segments, got {token:?}"
        );
        assert!(
            !segments[2].is_empty(),
            "case {case:?}: a signed JWS carries a signature segment, got {token:?}"
        );

        // The reading a relying party makes of a token its ISSUER handed it: it refuses the
        // unsecured form outright, so a token that gets past it is one something signed.
        let read = ::service_common::jwt::read(&token).unwrap_or_else(|defect| {
            panic!("case {case:?}: a relying party reads no token here: {defect}")
        });
        assert_eq!(
            read.algorithm, SIGNING_ALGORITHM,
            "case {case:?}: expected the header to name {SIGNING_ALGORITHM}, got {:?}",
            read.algorithm
        );
        let named_key = read.header.get("kid").and_then(Value::as_str);
        assert!(
            named_key.is_some_and(|kid| !kid.is_empty()),
            "case {case:?}: expected the header to name the signing key, got {:?}",
            read.header
        );

        // And the check itself: the signature verifies against the key the header names in
        // the key set this issuer publishes, over the token's own encoded header and payload.
        let verified = ::service_common::jwt::verify(&token, &published).unwrap_or_else(|defect| {
            panic!("case {case:?}: the vended token does not verify: {defect}")
        });
        assert_eq!(
            verified.claim_str("sub"),
            Some(identity_id.as_str()),
            "case {case:?}: expected the identity as subject, got {:?}",
            verified.claims
        );
        assert_eq!(
            verified.claim_str("aud"),
            Some(pool_id.as_str()),
            "case {case:?}: expected the pool as audience, got {:?}",
            verified.claims
        );

        // A signature is over the bytes that were signed: the same token carrying ANOTHER
        // payload verifies against nothing.
        let tampered = format!(
            "{}.{}.{}",
            segments[0],
            ::service_common::jwt::encode_base64url(
                json!({"iss": "https://cognito-identity.amazonaws.com", "sub": "rewritten"})
                    .to_string()
                    .as_bytes()
            ),
            segments[2]
        );
        assert!(
            ::service_common::jwt::verify(&tampered, &published).is_err(),
            "case {case:?}: a token whose payload was rewritten after signing must not verify"
        );
    }
}

/// RULE: the key set the issuer publishes carries the PUBLIC half of its signing key alone —
/// what a verifier needs and nothing more — under the `kid` the tokens it signed name.
#[tokio::test]
async fn the_published_key_set_carries_only_public_key_parameters() {
    let emulator = Emulator::new();
    let pool_id = pool(&emulator, "token-signature-keys").await;
    let (_, token) = minted(
        &emulator,
        "the published key set",
        &pool_id,
        Mint::ForIdentity,
        None,
        None,
    )
    .await;
    let published = token_signing::token_key_set();
    let keys = published["keys"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"the key set\": expected keys, got {published}"));
    assert!(
        !keys.is_empty(),
        "case \"the key set\": an issuer that signs publishes a key, got {published}"
    );
    let named_key = ::service_common::jwt::read(&token)
        .expect("case \"the key set\": the vended token reads as a compact JWS")
        .header
        .get("kid")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    assert!(
        keys.iter()
            .any(|key| key.get("kid").and_then(Value::as_str).map(ToString::to_string)
                == named_key),
        "case \"the key set\": expected the key the token names to be published, got {published}"
    );
    // Every private RSA parameter RFC 7518 §6.3.2 defines: none of them belongs in a
    // published entry.
    for parameter in ["d", "p", "q", "dp", "dq", "qi", "oth"] {
        assert!(
            keys.iter().all(|key| key.get(parameter).is_none()),
            "case {parameter:?}: a published entry carries no private parameter, got {published}"
        );
    }
}
