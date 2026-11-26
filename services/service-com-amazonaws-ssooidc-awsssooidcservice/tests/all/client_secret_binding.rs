// SPDX-License-Identifier: CC0-1.0

//! The pair `RegisterClient` answers with, as the registration's OWN credential.
//!
//! RULE: `RegisterClient` mints one registration per accepted call — its input names no
//! existing registration and carries no idempotency token (members `clientName`,
//! `clientType`, `scopes`, `redirectUris`, `grantTypes`, `issuerUrl`,
//! `entitledApplicationArn`, read with
//! `smithy_query(query="operation",operation="RegisterClient")`), and the secret is one
//! "generated for the client" that "the client will use to get authenticated by the
//! service in subsequent calls" — so the `clientId`/`clientSecret` the response carries
//! is that registration's own persisted credential: the response reports exactly what
//! the service stored for it, the pair goes on authenticating that registration for as
//! long as it is stored (authenticating with it neither rotates nor consumes it), no
//! other registration's secret authenticates it, and registering the same metadata
//! again mints a SEPARATE registration while leaving the earlier one and its pair
//! usable.
//!
//! RULE, the other half of the same sentence: the secret is one the SERVICE generates
//! for the registration, so nothing in the request determines it — a registration is
//! answered with a secret drawn for it alone, no function of the metadata it was
//! registered with (or of the `clientId` it stands beside) is that secret, and the
//! form of the value is the same whatever metadata the request carried.
//!
//! Every case drives the service through the router the production emulator serves,
//! using the harness the device-flow suite beside this file builds.

use std::collections::HashSet;

use serde_json::{json, Value};

use service_com_amazonaws_ssooidc_awsssooidcservice as ssooidc;
use ssooidc::state::{GRANT_TYPE_DEVICE_CODE, GRANT_TYPE_REFRESH_TOKEN};

/// The fixed `error_description` of an `invalid_client` refusal whose
/// `clientSecret` is no usable secret of the registration its `clientId` names,
/// written out so the case pins the wording itself.
const INVALID_CLIENT_SECRET_TEXT: &str = "Invalid client secret provided";

/// The fixed `error_description` of an `invalid_client` refusal whose
/// `clientSecret` is some OTHER registration's own, so the `clientId` standing
/// beside it is the member at fault.
const INVALID_CLIENT_ID_TEXT: &str = "Invalid client ID provided";

use crate::device_authorization_flow::{
    assert_failure, device_flow_token, start_url, text, Emulator,
};
// Every string a request carried, already read off a request body by the
// error-description suite in this same test binary.
use crate::error_descriptions::request_strings;

/// One registration's reported pair.
#[derive(Clone, Debug)]
struct Pair {
    client_id: String,
    client_secret: String,
}

/// Register `body` and report the pair the 200 carries.
async fn register(emulator: &Emulator, case: &str, body: &Value) -> Pair {
    let (status, response) = emulator.register_client(body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {response}"
    );
    Pair {
        client_id: text(&response, "clientId"),
        client_secret: text(&response, "clientSecret"),
    }
}

/// The secret the service PERSISTED for `client_id`, if it holds that registration.
fn stored_secret(emulator: &Emulator, client_id: &str) -> Option<String> {
    emulator
        .state()
        .lock()
        .clients
        .get(client_id)
        .map(|client| client.client_secret.clone())
}

/// Present `pair` to `StartDeviceAuthorization` and report the status and body: the
/// one operation that does nothing but authenticate the pair before it acts.
async fn authenticate(emulator: &Emulator, pair: &Pair) -> (u16, Value) {
    emulator
        .start_device_authorization(&json!({
            "clientId": pair.client_id,
            "clientSecret": pair.client_secret,
            "startUrl": start_url(),
        }))
        .await
}

/// The registration metadata the table spans: what the rule generalises over is the
/// metadata a registration carries — none beyond the required members, one grant, two
/// grants with scopes, redirect URIs with an issuer URL, and a name far longer than
/// any of them (`ClientName` is a plain string, no length or pattern constraint).
fn registrations() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a registration carrying only the required members",
            json!({"clientName": "aws-cli", "clientType": "public"}),
        ),
        (
            "a registration naming the device-code grant",
            json!({
                "clientName": "device-only",
                "clientType": "public",
                "grantTypes": [GRANT_TYPE_DEVICE_CODE],
            }),
        ),
        (
            "a registration naming two grants and a scope",
            json!({
                "clientName": "device-and-refresh",
                "clientType": "public",
                "grantTypes": [GRANT_TYPE_DEVICE_CODE, GRANT_TYPE_REFRESH_TOKEN],
                "scopes": ["sso:account:access"],
            }),
        ),
        (
            "a registration naming redirect URIs and an issuer URL",
            json!({
                "clientName": "pkce-client",
                "clientType": "public",
                "redirectUris": ["http://127.0.0.1:52000/oauth/callback"],
                "issuerUrl": "https://identitycenter.amazonaws.com/ssoins-1234567890abcdef",
            }),
        ),
        (
            "a registration with a name longer than any other row's",
            json!({"clientName": "a".repeat(512), "clientType": "public"}),
        ),
    ]
}

/// RULE (above): the reported pair is the registration's own persisted credential — the
/// response repeats what was stored, the pair keeps authenticating that registration,
/// and nothing but that registration's own secret does.
#[tokio::test]
async fn every_registration_is_reported_the_pair_the_service_persisted_for_it() {
    let emulator = Emulator::new();
    let mut seen_secrets: HashSet<String> = HashSet::new();

    for (case, body) in registrations() {
        let pair = register(&emulator, case, &body).await;
        assert!(
            seen_secrets.insert(pair.client_secret.clone()),
            "case {case:?}: a clientSecret was reported for a second registration"
        );

        // The member is not minted for the wire: it is what the store holds.
        assert_eq!(
            stored_secret(&emulator, &pair.client_id).as_deref(),
            Some(pair.client_secret.as_str()),
            "case {case:?}: the reported clientSecret is not the one persisted for {}",
            pair.client_id
        );

        // Authenticating with it is not what mints it, so it holds across calls.
        for attempt in 1..=3 {
            let (status, body) = authenticate(&emulator, &pair).await;
            assert_eq!(
                status, 200,
                "case {case:?}: attempt {attempt} with the registration's own pair was refused: {body}"
            );
            assert_eq!(
                stored_secret(&emulator, &pair.client_id).as_deref(),
                Some(pair.client_secret.as_str()),
                "case {case:?}: attempt {attempt} changed the stored secret"
            );
        }

        // A secret no registration carries is refused as the secret it is; the
        // `clientId` beside it is a registration this service holds.
        let fabricated = Pair {
            client_id: pair.client_id.clone(),
            client_secret: format!("{}-not-the-stored-secret", pair.client_secret),
        };
        let (status, response) = authenticate(&emulator, &fabricated).await;
        assert_failure(
            case,
            status,
            &response,
            401,
            "InvalidClientException",
            "invalid_client",
        );
        assert_eq!(
            response["error_description"].as_str(),
            Some(INVALID_CLIENT_SECRET_TEXT),
            "case {case:?}: expected the fixed text for a secret of no registration, got {response}"
        );
    }
}

/// RULE (above): the same metadata registered again is a SEPARATE registration — a
/// fresh pair, the earlier registration untouched and still authenticating, and
/// neither registration's secret authenticating the other's `clientId`.
#[tokio::test]
async fn registering_the_same_metadata_again_leaves_the_earlier_registration_intact() {
    let emulator = Emulator::new();

    for (case, body) in registrations() {
        let first = register(&emulator, case, &body).await;
        let second = register(&emulator, case, &body).await;

        assert_ne!(
            first.client_id, second.client_id,
            "case {case:?}: the second registration reused the first clientId {}",
            first.client_id
        );
        assert_ne!(
            first.client_secret, second.client_secret,
            "case {case:?}: the second registration of the same metadata reported the first secret"
        );

        // Both registrations are held, each with its own secret.
        for (which, pair) in [("first", &first), ("second", &second)] {
            assert_eq!(
                stored_secret(&emulator, &pair.client_id).as_deref(),
                Some(pair.client_secret.as_str()),
                "case {case:?}: the {which} registration's stored secret is not the reported one"
            );
            let (status, response) = authenticate(&emulator, pair).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the {which} registration's pair was refused: {response}"
            );
        }

        // A secret this service issued, presented beside the OTHER registration's
        // `clientId`, is refused for the id standing beside it.
        for (which, crossed) in [
            (
                "the first id with the second secret",
                Pair {
                    client_id: first.client_id.clone(),
                    client_secret: second.client_secret.clone(),
                },
            ),
            (
                "the second id with the first secret",
                Pair {
                    client_id: second.client_id.clone(),
                    client_secret: first.client_secret.clone(),
                },
            ),
        ] {
            let (status, response) = authenticate(&emulator, &crossed).await;
            assert_failure(
                &format!("{case}: {which}"),
                status,
                &response,
                401,
                "InvalidClientException",
                "invalid_client",
            );
            assert_eq!(
                response["error_description"].as_str(),
                Some(INVALID_CLIENT_ID_TEXT),
                "case {case:?}: {which}: expected the fixed text for the id beside another registration's secret, got {response}"
            );
        }
    }
}

/// RULE (above): the pair the response carries is the credential the whole flow runs
/// on — the device-code grant and the refresh-token grant both authenticate with it,
/// and it is still the registration's pair afterwards.
#[tokio::test]
async fn the_reported_pair_authenticates_the_grants_that_follow_the_registration() {
    let emulator = Emulator::new();

    for (case, body) in registrations() {
        let pair = register(&emulator, case, &body).await;
        let token = device_flow_token(&emulator, &pair.client_id, &pair.client_secret).await;
        assert!(
            !text(&token, "accessToken").is_empty(),
            "case {case:?}: expected an accessToken for the registration's own pair, got {token}"
        );

        // "This list is used to restrict the token granting flows available to the
        // client" (`RegisterClientRequest$grantTypes`), so a registration is handed
        // a refresh token exactly when it named that grant — or named no grant types
        // at all, which restricts nothing.
        let declares_refresh = body["grantTypes"].as_array().is_none_or(|grants| {
            grants.is_empty() || grants.iter().any(|grant| grant == GRANT_TYPE_REFRESH_TOKEN)
        });
        let refresh_token = token["refreshToken"].as_str().map(str::to_string);
        assert_eq!(
            refresh_token.is_some(),
            declares_refresh,
            "case {case:?}: expected a refreshToken beside the access token: {declares_refresh}, got {token}"
        );

        // The refresh token it was handed is rotated for the SAME pair, which
        // authenticates the refresh too.
        if let Some(refresh_token) = refresh_token {
            let (status, rotated) = emulator
                .create_token(&json!({
                    "clientId": pair.client_id,
                    "clientSecret": pair.client_secret,
                    "grantType": GRANT_TYPE_REFRESH_TOKEN,
                    "refreshToken": refresh_token,
                }))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the refresh with the registration's own pair was refused: {rotated}"
            );
        }

        assert_eq!(
            stored_secret(&emulator, &pair.client_id).as_deref(),
            Some(pair.client_secret.as_str()),
            "case {case:?}: the flow changed the registration's stored secret"
        );
    }
}

/// A digest of `input`, in hex. `sha2` and `hex` are this crate's own
/// dev-dependencies; they are used here only to BUILD a candidate secret a caller
/// could compute for itself, never to check one.
fn digest_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};

    hex::encode(Sha256::digest(input.as_bytes()))
}

/// What a caller holding nothing but the registration's own metadata and the
/// `clientId` it was answered with could put in `clientSecret`: the metadata
/// itself, a digest of it, the id standing beside it, and no secret at all.
fn computed_candidates(body: &Value, pair: &Pair) -> Vec<(&'static str, String)> {
    let client_name = body["clientName"].as_str().unwrap_or_default().to_string();
    vec![
        ("the client name itself", client_name.clone()),
        ("a digest of the client name", digest_hex(&client_name)),
        (
            "a digest of the whole registration metadata",
            digest_hex(&body.to_string()),
        ),
        (
            "the clientId the secret was answered beside",
            pair.client_id.clone(),
        ),
        ("a digest of that clientId", digest_hex(&pair.client_id)),
        ("no secret at all", String::new()),
    ]
}

/// RULE (above): the secret is GENERATED for the registration, so nothing in the
/// request determines it — the same metadata registered again and again is answered
/// with a secret never seen before, the secret is not the `clientId` it stands beside
/// and carries no part of the metadata, and its form does not vary with the metadata
/// so it reports nothing about the registration it belongs to.
#[tokio::test]
async fn nothing_in_a_registration_request_determines_the_secret_it_is_answered_with() {
    /// How many times each row of the table is registered: enough that a secret
    /// computed from the metadata would have to repeat.
    const ROUNDS: usize = 4;
    /// A credential nobody can guess is not a handful of characters. The FLOOR is
    /// asserted, not this emulator's own width: `ClientSecret` is a plain string
    /// with no `@length` and no `@pattern`
    /// (`smithy_query(query="shape",shape="ClientSecret")`), so the model states no
    /// width to pin.
    const MINIMUM_WIDTH: usize = 32;

    let emulator = Emulator::new();
    let mut seen_secrets: HashSet<String> = HashSet::new();
    let mut widths: HashSet<usize> = HashSet::new();

    for (case, body) in registrations() {
        // Everything about the registration a caller could try to compute its
        // secret from: every string the request carried.
        let carried = request_strings(&body);
        for round in 1..=ROUNDS {
            let pair = register(&emulator, case, &body).await;
            let secret = pair.client_secret.clone();

            assert!(
                seen_secrets.insert(secret.clone()),
                "case {case:?}: round {round} was answered a clientSecret another registration was already answered with"
            );
            assert!(
                secret.len() >= MINIMUM_WIDTH,
                "case {case:?}: round {round}: expected a clientSecret of at least {MINIMUM_WIDTH} characters, got {} in {secret:?}",
                secret.len()
            );
            assert_ne!(
                secret, pair.client_id,
                "case {case:?}: round {round}: the clientSecret is the clientId it stands beside"
            );
            for value in carried.iter().filter(|value| !value.is_empty()) {
                assert!(
                    !secret.contains(value.as_str()),
                    "case {case:?}: round {round}: the clientSecret carries the request's {value:?}: {secret:?}"
                );
                assert!(
                    !value.contains(secret.as_str()),
                    "case {case:?}: round {round}: the request's {value:?} carries the clientSecret {secret:?}"
                );
            }
            widths.insert(secret.len());
        }
    }

    // What is asserted here is that the width does not VARY with the metadata, so the
    // value reports nothing about the registration it belongs to. That this emulator
    // draws one fixed width is its own choice — the model states none — so the width
    // itself is not written out.
    assert_eq!(
        widths.len(),
        1,
        "expected one clientSecret width whatever metadata a registration carried, got {widths:?}"
    );
}

/// RULE (above): only the secret the service drew for the registration authenticates
/// it, so a secret a caller COMPUTED from that registration is the secret of no
/// registration at all — refused as the member at fault, and refusing it leaves the
/// registration's own credential untouched.
#[tokio::test]
async fn no_secret_computed_from_a_registration_authenticates_it() {
    let emulator = Emulator::new();

    for (case, body) in registrations() {
        let pair = register(&emulator, case, &body).await;

        for (which, candidate) in computed_candidates(&body, &pair) {
            let computed = Pair {
                client_id: pair.client_id.clone(),
                client_secret: candidate,
            };
            let (status, response) = authenticate(&emulator, &computed).await;
            assert_failure(
                &format!("{case}: {which}"),
                status,
                &response,
                401,
                "InvalidClientException",
                "invalid_client",
            );
            assert_eq!(
                response["error_description"].as_str(),
                Some(INVALID_CLIENT_SECRET_TEXT),
                "case {case:?}: {which}: expected the fixed text for a secret of no registration, got {response}"
            );
        }

        // A refused candidate neither rotates nor consumes the credential: the
        // registration's own pair still authenticates it, and still is what the
        // service holds for it.
        let (status, response) = authenticate(&emulator, &pair).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the registration's own pair was refused after the computed candidates: {response}"
        );
        assert_eq!(
            stored_secret(&emulator, &pair.client_id).as_deref(),
            Some(pair.client_secret.as_str()),
            "case {case:?}: a refused candidate changed the registration's stored secret"
        );
    }
}
