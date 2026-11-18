// SPDX-License-Identifier: CC0-1.0

//! How the OAuth2 credential-provider routes word a `name` that resolves to no OAuth2
//! provider of this account's token vault.
//!
//! The rule under test: the refusal sentence belongs to the route that answers it, not to
//! a template shared across the credential-provider planes — the OAuth2 routes answer one
//! ENTIRELY FIXED sentence that names neither the provider kind asked for nor the name the
//! request carried, so no caller can read a name's existence (in this plane or in any
//! other) off the words it gets back. The API-key read beside it does name the value its
//! request carried, which is why the OAuth2 sentence cannot be derived from it.
//!
//! The rows span why the name resolves to nothing — never used at all, used by a provider
//! of ANOTHER plane, a held OAuth2 provider's name in another letter case, and a name
//! whose only create was refused — and the three OAuth2 routes that address a provider by
//! name (`Get`, `Update` and `Delete`), so the sentence is the plane's rather than one
//! operation's.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a managed provider's credential is
//! stored in a real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region every request in this suite is signed for.
const REGION: &str = "eu-central-1";

/// The whole of the sentence every OAuth2 credential-provider route answers for a name
/// that resolves to no provider of that plane. Written out rather than computed, so the
/// expectation pins the words a caller reads.
const OAUTH2_NOT_FOUND: &str = "CredentialProvider not found";

/// The two documents `Oauth2Discovery$discoveryUrl` admits, which are the published
/// locations of an authorization server's metadata (OpenID Connect Discovery 1.0 and RFC
/// 8414).
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// POST one `restJson1` operation of this service, signed for [`REGION`].
    async fn post(&self, uri: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", authorization)
            .header("host", "bedrock-agentcore-control.amazonaws.com")
            .header("content-type", "application/json")
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A `CustomOauth2` configuration, whose authorization server is the one the request
/// names.
fn custom_config(client: &str) -> Value {
    json!({"customOauth2ProviderConfig": {
        "oauthDiscovery": {"discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}")},
        "clientId": client,
        "clientSecret": "a-secret",
    }})
}

/// Every OAuth2 route that addresses a provider by `name`, as (route, the request body for
/// `name`): the read, the update and the delete.
fn oauth2_routes(name: &str) -> [(&'static str, Value); 3] {
    [
        (
            "/identities/GetOauth2CredentialProvider",
            json!({"name": name}),
        ),
        (
            "/identities/UpdateOauth2CredentialProvider",
            json!({
                "name": name,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": custom_config("a-client"),
            }),
        ),
        (
            "/identities/DeleteOauth2CredentialProvider",
            json!({"name": name}),
        ),
    ]
}

/// Every OAuth2 route refuses a name that resolves to no OAuth2 provider with ONE fixed
/// sentence, whatever the name and whatever else holds it.
#[tokio::test]
async fn every_oauth2_route_refuses_an_absent_name_in_one_fixed_sentence() {
    let emulator = Emulator::new();

    // A provider of ANOTHER plane under a name of its own, so a row can ask the OAuth2
    // plane for a name this account really does hold — in the other plane.
    let api_key_name = "shared-provider-name";
    let (status, created) = emulator
        .post(
            "/identities/CreateApiKeyCredentialProvider",
            &json!({"name": api_key_name, "apiKey": "a-key"}),
        )
        .await;
    assert_eq!(
        status, 201,
        "the API-key provider the isolation rows ask for must be created, got {status} \
         ({created})"
    );

    // One OAuth2 provider this account does hold, so a row can address it in another
    // letter case.
    let held = "held-oauth2-provider";
    let (status, created) = emulator
        .post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": held,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": custom_config("held-client"),
            }),
        )
        .await;
    assert_eq!(
        status, 201,
        "the OAuth2 provider the case-sensitivity row addresses must be created, got {status} \
         ({created})"
    );

    // A create whose vendor and configuration disagree leaves no provider behind, so this
    // name is one nothing stands under either.
    let (status, refused) = emulator
        .post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": "refused-oauth2-provider",
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": custom_config("a-client"),
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "the setup create is refused, so its name holds no provider, got {status} ({refused})"
    );

    // (case, the name the route addresses)
    let rows: [(&str, &str); 4] = [
        ("a name never used", "no-such-provider"),
        ("a name only another plane holds", api_key_name),
        ("the held provider's name in another letter case", "Held-Oauth2-Provider"),
        ("a name whose only create was refused", "refused-oauth2-provider"),
    ];

    for (case, name) in rows {
        for (route, body) in oauth2_routes(name) {
            let (status, refused) = emulator.post(route, &body).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?} on {route}: expected HTTP 404 ResourceNotFoundException, got \
                 {status} ({refused})"
            );
            assert_eq!(
                message(&refused),
                OAUTH2_NOT_FOUND,
                "case {case:?} on {route}: unexpected refusal message, got {refused}"
            );
            assert!(
                !message(&refused).contains(name),
                "case {case:?} on {route}: the refusal echoes the requested name, got {refused}"
            );
        }
    }

    // The provider of the other plane is untouched by the OAuth2 routes that were refused
    // for its name — including the delete.
    let (status, fetched) = emulator
        .post(
            "/identities/GetApiKeyCredentialProvider",
            &json!({"name": api_key_name}),
        )
        .await;
    assert_eq!(
        (status, fetched["name"].clone()),
        (200, json!(api_key_name)),
        "the other plane's provider must still be held, got {status} ({fetched})"
    );

    // And the OAuth2 provider this account holds is still readable under its own name, so
    // the refusals above are about the name, not about the plane being empty.
    let (status, fetched) = emulator
        .post(
            "/identities/GetOauth2CredentialProvider",
            &json!({"name": held}),
        )
        .await;
    assert_eq!(
        (status, fetched["name"].clone()),
        (200, json!(held)),
        "the held OAuth2 provider must still be readable, got {status} ({fetched})"
    );
}

/// The API-key read's own refusal is a DIFFERENT sentence, which names the value its
/// request carried: the two planes word their refusals themselves, so neither is derived
/// from the other.
#[tokio::test]
async fn the_api_key_read_words_its_own_refusal_and_names_the_requested_name() {
    let emulator = Emulator::new();

    // (case, the name the read addresses)
    let rows: [(&str, &str); 2] = [
        ("a name never used", "no-such-api-key-provider"),
        ("a name only the OAuth2 plane holds", "oauth2-only-provider"),
    ];
    let (status, created) = emulator
        .post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": "oauth2-only-provider",
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": custom_config("a-client"),
            }),
        )
        .await;
    assert_eq!(
        status, 201,
        "the OAuth2 provider the second row asks the API-key plane for must be created, got \
         {status} ({created})"
    );

    for (case, name) in rows {
        let (status, refused) = emulator
            .post(
                "/identities/GetApiKeyCredentialProvider",
                &json!({"name": name}),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} ({refused})"
        );
        assert_eq!(
            message(&refused),
            format!("ApiKeyCredentialProvider not found for {name}"),
            "case {case:?}: unexpected refusal message, got {refused}"
        );
    }
}
