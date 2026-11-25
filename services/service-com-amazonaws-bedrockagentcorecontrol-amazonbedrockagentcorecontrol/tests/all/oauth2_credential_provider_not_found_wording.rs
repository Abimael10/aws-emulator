// SPDX-License-Identifier: CC0-1.0

//! How each OAuth2 credential-provider route words a `name` that resolves to no provider
//! of this account's token vault.
//!
//! The rule under test: the not-found sentence belongs to the ROUTE that answers it, so the
//! routes of this family that only report or retire a provider answer one entirely fixed
//! sentence that names neither the provider kind nor the value the request carried, while
//! the UPDATE route adds the member and value it could not resolve — the input the caller
//! has to correct — whatever that value is.
//!
//! The rows span why a name resolves to nothing (never created, created and then deleted,
//! a held provider's name in another letter case, a name whose only create was refused)
//! and what the name LOOKS like (the shortest `CredentialProviderName` the model admits,
//! one at its 128-character ceiling, and one spending the whole `^[a-zA-Z0-9\-_]+$`
//! character class, from `smithy_query shape=CredentialProviderName`), because the
//! update's sentence carries the request's value and so must not depend on its shape.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a managed provider's client secret is
//! stored in a real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region every request in this suite is signed for.
const REGION: &str = "us-west-2";

/// The whole of the sentence the routes that report or retire a provider answer, written
/// out rather than computed so the expectation pins the words a caller reads.
const NOT_FOUND: &str = "CredentialProvider not found";

/// The published location of an authorization server's metadata document, which
/// `Oauth2Discovery$discoveryUrl` admits (OpenID Connect Discovery 1.0).
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

    async fn create(&self, name: &str) -> (u16, Value) {
        self.post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": name,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": {
                    "clientId": "a-client",
                    "clientSecret": "a-secret",
                }},
            }),
        )
        .await
    }

    async fn get(&self, name: &str) -> (u16, Value) {
        self.post(
            "/identities/GetOauth2CredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    async fn update(&self, name: &str) -> (u16, Value) {
        self.post(
            "/identities/UpdateOauth2CredentialProvider",
            &json!({
                "name": name,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": {
                    "clientId": "another-client",
                    "clientSecret": "another-secret",
                }},
            }),
        )
        .await
    }

    async fn delete(&self, name: &str) -> (u16, Value) {
        self.post(
            "/identities/DeleteOauth2CredentialProvider",
            &json!({"name": name}),
        )
        .await
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

/// A name at `CredentialProviderName`'s 128-character ceiling (`smithy_query
/// shape=CredentialProviderName` reports `@length 1..128`), so the update's sentence is
/// pinned on the longest value a request can carry.
fn longest_name() -> String {
    "a".repeat(128)
}

/// The names this suite asks every route about, as (case, the name), each reaching the
/// not-found path for a different reason or through a differently shaped value. The
/// emulator is prepared by [`prepare`] so each row really resolves to nothing.
fn absent_names(longest: &str) -> Vec<(&'static str, String)> {
    vec![
        ("a name never used", "no-such-provider".to_string()),
        (
            "a name whose provider was deleted",
            "deleted-provider".to_string(),
        ),
        (
            "a held provider's name in another letter case",
            "Held-Provider".to_string(),
        ),
        (
            "a name whose only create was refused",
            "refused-provider".to_string(),
        ),
        ("the shortest name the model admits", "a".to_string()),
        ("a name at the model's length ceiling", longest.to_string()),
        (
            "a name spending the whole character class",
            "Ab-9_zZ-0_cD".to_string(),
        ),
    ]
}

/// Bring the emulator to the state [`absent_names`] describes: one provider held under a
/// name of its own, one created and then deleted, and one name whose only create was
/// refused. Returns the name of the provider that is still held.
async fn prepare(emulator: &Emulator) -> &'static str {
    let held = "held-provider";
    let (status, created) = emulator.create(held).await;
    assert_eq!(
        status, 201,
        "the provider the letter-case row addresses must be created, got {status} ({created})"
    );

    let (status, created) = emulator.create("deleted-provider").await;
    assert_eq!(
        status, 201,
        "the provider the deleted row addresses must be created first, got {status} ({created})"
    );
    let (status, deleted) = emulator.delete("deleted-provider").await;
    assert_eq!(
        status, 204,
        "the delete of that provider must succeed, got {status} ({deleted})"
    );

    // A create whose declared vendor and supplied configuration member disagree leaves no
    // provider behind, so this name holds nothing either.
    let (status, refused) = emulator
        .post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": "refused-provider",
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"customOauth2ProviderConfig": {
                    "oauthDiscovery": {"discoveryUrl":
                        format!("https://auth.example.com{OPENID_CONFIGURATION}")},
                    "clientId": "a-client",
                    "clientSecret": "a-secret",
                }},
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "the setup create must be refused so its name holds no provider, got {status} \
         ({refused})"
    );

    held
}

/// The READ refuses every absent name with the family's fixed sentence, which says nothing
/// about the value the request carried.
#[tokio::test]
async fn the_read_refuses_an_absent_name_without_naming_it() {
    let emulator = Emulator::new();
    let held = prepare(&emulator).await;
    let longest = longest_name();

    for (case, name) in absent_names(&longest) {
        let (status, refused) = emulator.get(&name).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} \
             ({refused})"
        );
        // Equality with the fixed sentence is itself the statement that nothing of the
        // request reached the words a caller reads.
        assert_eq!(
            message(&refused),
            NOT_FOUND,
            "case {case:?}: expected the family's fixed sentence, got {refused}"
        );
    }

    // The refusals above are about the name, not about an empty plane.
    let (status, fetched) = emulator.get(held).await;
    assert_eq!(
        (status, fetched["name"].clone()),
        (200, json!(held)),
        "the held provider must still be readable, got {status} ({fetched})"
    );
}

/// The UPDATE refuses every absent name with the family's sentence PLUS the member and
/// value it could not resolve, whatever that value is.
#[tokio::test]
async fn the_update_refuses_an_absent_name_and_names_the_value_it_could_not_resolve() {
    let emulator = Emulator::new();
    let held = prepare(&emulator).await;
    let longest = longest_name();

    for (case, name) in absent_names(&longest) {
        let (status, refused) = emulator.update(&name).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} \
             ({refused})"
        );
        assert_eq!(
            message(&refused),
            format!("{NOT_FOUND} for name: {name}"),
            "case {case:?}: expected the update route's own sentence, got {refused}"
        );

        // A refused update mints nothing: the name still resolves to no provider.
        let (status, fetched) = emulator.get(&name).await;
        assert_eq!(
            (status, error_code(&fetched).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: the refused update left a provider behind, got {status} \
             ({fetched})"
        );
    }

    // An update of the name this account DOES hold is still served, so the refusals above
    // are about the name rather than about the route.
    let (status, updated) = emulator.update(held).await;
    assert_eq!(
        (status, updated["name"].clone()),
        (200, json!(held)),
        "the held provider must still be updatable, got {status} ({updated})"
    );
}

/// The recorded lifecycle, end to end on one name: create, read, delete, then the two
/// routes' different sentences for the very same now-absent name.
#[tokio::test]
async fn a_deleted_provider_s_name_gets_each_route_s_own_sentence() {
    let emulator = Emulator::new();
    let name = "lifecycle-provider";

    let (status, created) = emulator.create(name).await;
    assert_eq!(
        status, 201,
        "the provider under test must be created, got {status} ({created})"
    );
    let (status, fetched) = emulator.get(name).await;
    assert_eq!(
        (status, fetched["name"].clone()),
        (200, json!(name)),
        "the created provider must be readable, got {status} ({fetched})"
    );
    let (status, updated) = emulator.update(name).await;
    assert_eq!(
        status, 200,
        "the held provider's update must be served, got {status} ({updated})"
    );
    let (status, deleted) = emulator.delete(name).await;
    assert_eq!(
        status, 204,
        "the delete must succeed, got {status} ({deleted})"
    );

    let (status, refused) = emulator.get(name).await;
    assert_eq!(
        (status, error_code(&refused).as_str(), message(&refused)),
        (404, "ResourceNotFoundException", NOT_FOUND.to_string()),
        "the read of the deleted provider: unexpected refusal, got {status} ({refused})"
    );

    let (status, refused) = emulator.update(name).await;
    assert_eq!(
        (status, error_code(&refused).as_str(), message(&refused)),
        (
            404,
            "ResourceNotFoundException",
            format!("{NOT_FOUND} for name: {name}")
        ),
        "the update of the deleted provider: unexpected refusal, got {status} ({refused})"
    );
}
