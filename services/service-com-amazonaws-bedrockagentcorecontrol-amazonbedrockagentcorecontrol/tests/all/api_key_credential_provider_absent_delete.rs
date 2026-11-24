// SPDX-License-Identifier: CC0-1.0

//! What `DeleteApiKeyCredentialProvider` answers for a `name` that resolves to no API-key
//! credential provider of this account, whatever made the name resolve to nothing.
//!
//! The rule under test, read off the Smithy model rather than off any one request
//! (`smithy_query(query="operation",operation="DeleteApiKeyCredentialProvider,\
//! GetApiKeyCredentialProvider,GetOauth2CredentialProvider")`): this delete is satisfied by
//! the provider's ABSENCE from its own plane. The model declares it idempotent with an
//! empty 204 success, so a name this account holds no API-key provider under is answered
//! with that same empty success rather than a refusal — however often it is repeated — and
//! the call removes nothing: it reaches neither the API-key providers held under other
//! names nor a provider of ANOTHER credential-provider plane that happens to hold the name
//! the request carried. The modelled `ResourceNotFoundException` (HTTP 404) stays on the
//! route that has to RESOLVE the provider to answer at all — the read — worded with the
//! name the request carried.
//!
//! The rows differ along WHY the name resolves to no API-key provider: never used at all,
//! used only by a provider of another plane, a held provider's name in another letter case,
//! a name whose only create was refused, and the shortest and longest names the member's
//! constraints admit. A delete naming a provider the account really does hold is the
//! positive control.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a managed provider's key is stored in a
//! real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region every request in this suite is signed for.
const REGION: &str = "ap-south-1";

/// The longest name `CredentialProviderName` admits: length 1..128, pattern
/// `^[a-zA-Z0-9\-_]+$` (`smithy_query(query="shape",shape="CredentialProviderName")`).
const MAX_PROVIDER_NAME_LEN: usize = 128;

/// The published location of an authorization server's metadata (OpenID Connect Discovery
/// 1.0), which `Oauth2Discovery$discoveryUrl` admits — needed only to stand an OAuth2
/// provider up as the other plane's holder of a name.
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

/// The emulator the rows run against: the control plane, and Secrets Manager beside it on
/// the one shared engine.
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

    /// POST one `restJson1` operation of this service, signed for [`REGION`] — which is what
    /// the router routes on.
    async fn post(&self, uri: &str, body: &Value) -> Answer {
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
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    async fn create_api_key_provider(&self, body: &Value) -> Answer {
        self.post("/identities/CreateApiKeyCredentialProvider", body)
            .await
    }

    async fn delete_api_key_provider(&self, name: &str) -> Answer {
        self.post(
            "/identities/DeleteApiKeyCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    async fn get_api_key_provider(&self, name: &str) -> Answer {
        self.post(
            "/identities/GetApiKeyCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    async fn get_oauth2_provider(&self, name: &str) -> Answer {
        self.post(
            "/identities/GetOauth2CredentialProvider",
            &json!({"name": name}),
        )
        .await
    }
}

/// One answer from the emulator: its status and its JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The error code `restJson1` carries in `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The sentence the API-key READ route refuses an unresolvable name with. The fixed text is
/// the service's; the only variable part is the name the request carried.
fn not_found_message(name: &str) -> String {
    format!("ApiKeyCredentialProvider not found for {name}")
}

/// Assert one answer is the delete's own success: HTTP 204 and, because the modelled output
/// shape has no members, a body carrying none.
fn assert_empty_success(case: &str, answer: &Answer) {
    let carries_no_member = match &answer.body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    };
    assert!(
        answer.status == 204 && carries_no_member,
        "case {case:?}: expected HTTP 204 carrying no member, got {} ({})",
        answer.status,
        answer.body
    );
}

/// A `CustomOauth2` configuration, whose authorization server is the one the request names.
fn custom_oauth2_config() -> Value {
    json!({"customOauth2ProviderConfig": {
        "oauthDiscovery": {
            "discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}"),
        },
        "clientId": "a-client",
        "clientSecret": "a-secret",
    }})
}

/// A delete of ANY name this account holds no API-key provider under is the delete's own
/// empty 204 success, repeatable, and removes nothing: the API-key provider held under
/// another name, and the OAuth2 provider that holds one of the requested names in its own
/// plane, are both still readable afterwards. The name stays unresolvable on the read route,
/// which is where the modelled 404 lives, and is free for a new provider.
#[tokio::test]
async fn an_api_key_provider_delete_that_resolves_nothing_succeeds_and_removes_nothing() {
    let emulator = Emulator::new();

    // The API-key provider no row names: the positive control that this account's own
    // providers survive every refusal-free no-op below, and the holder of the name a row
    // addresses in another letter case.
    const HELD: &str = "held-api-key-provider";
    let created = emulator
        .create_api_key_provider(&json!({"name": HELD, "apiKey": "sk-held-0001"}))
        .await;
    assert_eq!(
        created.status, 201,
        "setup: the held API-key provider must be created, got {} ({})",
        created.status, created.body
    );

    // A provider of ANOTHER credential-provider plane, so a row can delete a name this
    // account really does hold — in a plane this delete does not address.
    const OAUTH2_ONLY: &str = "oauth2-only-provider";
    let other_plane = emulator
        .post(
            "/identities/CreateOauth2CredentialProvider",
            &json!({
                "name": OAUTH2_ONLY,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": custom_oauth2_config(),
            }),
        )
        .await;
    assert_eq!(
        other_plane.status, 201,
        "setup: the other plane's provider must be created, got {} ({})",
        other_plane.status, other_plane.body
    );

    // A create that references a secret this emulator does not hold is refused, so its name
    // holds no provider either — a name the account has seen and still does not hold.
    const REFUSED_CREATE: &str = "refused-api-key-provider";
    let refused = emulator
        .create_api_key_provider(&json!({
            "name": REFUSED_CREATE,
            "apiKeySecretSource": "EXTERNAL",
            "apiKeySecretConfig": {"secretId": "no/such/secret", "jsonKey": "theKey"},
        }))
        .await;
    assert_eq!(
        refused.status, 404,
        "setup: the create referencing an absent secret is refused, got {} ({})",
        refused.status, refused.body
    );

    // Every name below satisfies `CredentialProviderName` (length 1..128, pattern
    // `^[a-zA-Z0-9\-_]+$`), so what each row exercises is the handler's own lookup rather
    // than the boundary constraint walk. (case, the name the delete carries)
    let rows: [(&str, String); 6] = [
        ("a name never used", "no-such-api-key-provider".to_string()),
        ("a name only another plane holds", OAUTH2_ONLY.to_string()),
        (
            "the held provider's name in another letter case",
            "Held-Api-Key-Provider".to_string(),
        ),
        (
            "a name whose only create was refused",
            REFUSED_CREATE.to_string(),
        ),
        ("the shortest name the pattern admits", "x".to_string()),
        (
            "the longest name the pattern admits",
            format!("{:_>width$}", "Q", width = MAX_PROVIDER_NAME_LEN),
        ),
    ];

    for (case, name) in &rows {
        // Three times: the delete is satisfied by the provider's absence, so it answers the
        // same empty success however often it is repeated on a name nothing resolves.
        for attempt in 1..=3 {
            let answered = emulator.delete_api_key_provider(name).await;
            assert_empty_success(&format!("{case} ({name:?}), attempt {attempt}"), &answered);
        }

        // The name the delete accepted still resolves no API-key provider, and the refusal
        // the READ answers names the name the request carried.
        let read = emulator.get_api_key_provider(name).await;
        assert_eq!(
            (read.status, read.error_code(), read.message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message(name)
            ),
            "case {case:?} ({name:?}): the read refuses, naming the requested name, got {} \
             ({})",
            read.status,
            read.body
        );
    }

    // Nothing the account holds was removed by any of those calls.
    let intact = emulator.get_api_key_provider(HELD).await;
    assert_eq!(
        (intact.status, intact.body["name"].clone()),
        (200, json!(HELD)),
        "the account's own API-key provider is still readable, got {} ({})",
        intact.status,
        intact.body
    );
    let other = emulator.get_oauth2_provider(OAUTH2_ONLY).await;
    assert_eq!(
        (other.status, other.body["name"].clone()),
        (200, json!(OAUTH2_ONLY)),
        "the other plane's provider under a deleted name is still readable, got {} ({})",
        other.status,
        other.body
    );

    // And the no-op left no tombstone behind: each name is free for a new provider, which
    // then reads back under that name.
    for (case, name) in &rows {
        if name == OAUTH2_ONLY {
            // Held in the other plane already; creating an API-key provider under it is the
            // isolation the suite above pins, not this one's concern.
            continue;
        }
        let created = emulator
            .create_api_key_provider(&json!({"name": name, "apiKey": "sk-reused-0002"}))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?} ({name:?}): the name the no-op accepted is free for a new \
             provider, got {} ({})",
            created.status, created.body
        );
        let read = emulator.get_api_key_provider(name).await;
        assert_eq!(
            (read.status, read.body["name"].clone()),
            (200, json!(name)),
            "case {case:?} ({name:?}): the new provider reads back, got {} ({})",
            read.status,
            read.body
        );
    }
}

/// The positive control beside the rows above: a delete that DOES resolve a provider answers
/// the very same empty 204 success, and only then does the account stop holding it — so the
/// success the absent names get is not the delete failing silently.
#[tokio::test]
async fn an_api_key_provider_delete_that_resolves_a_provider_answers_the_same_success() {
    let emulator = Emulator::new();

    const NAME: &str = "retired-api-key-provider";
    const NEIGHBOUR: &str = "neighbour-api-key-provider";
    for name in [NAME, NEIGHBOUR] {
        let created = emulator
            .create_api_key_provider(&json!({"name": name, "apiKey": "sk-control-0003"}))
            .await;
        assert_eq!(
            created.status, 201,
            "setup: {name:?} must be created, got {} ({})",
            created.status, created.body
        );
    }

    let answered = emulator.delete_api_key_provider(NAME).await;
    assert_empty_success("a delete that resolves a provider", &answered);

    let read = emulator.get_api_key_provider(NAME).await;
    assert_eq!(
        (read.status, read.error_code(), read.message()),
        (
            404,
            "ResourceNotFoundException".to_string(),
            not_found_message(NAME)
        ),
        "the deleted provider stops resolving, got {} ({})",
        read.status,
        read.body
    );

    let repeated = emulator.delete_api_key_provider(NAME).await;
    assert_empty_success("the repeat of a delete that resolved a provider", &repeated);

    let neighbour = emulator.get_api_key_provider(NEIGHBOUR).await;
    assert_eq!(
        (neighbour.status, neighbour.body["name"].clone()),
        (200, json!(NEIGHBOUR)),
        "the neighbouring provider is untouched, got {} ({})",
        neighbour.status,
        neighbour.body
    );
}
