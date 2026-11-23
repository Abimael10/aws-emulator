// SPDX-License-Identifier: CC0-1.0

//! What `UpdateOauth2CredentialProvider`'s own answer REPRESENTS.
//!
//! RULE under test: a mutation's response reports the resource the mutation left stored —
//! every optional output member carries the stored value where the resource has one, and is
//! ABSENT where nothing configured it; no member is synthesized to fill a gap. For this
//! provider plane that is two members of the update's own envelope
//! (`smithy_query(query="operation",operation="UpdateOauth2CredentialProvider")`: `callbackUrl`
//! (String) and `clientSecretJsonKey` (`SecretJsonKeyType`, `@length` 1..128) are both
//! optional): the callback URL the provider was published with is reported on every update,
//! and the client secret's JSON key is reported exactly when the configuration the update
//! carried named one in its own `SecretReference` — `SecretReference$jsonKey` is `@required`
//! inside that reference and the reference itself is "Required when `clientSecretSource` is
//! set to `EXTERNAL`" (`smithy_query(query="shape",shape="SecretReference,\
//! GoogleOauth2ProviderConfigInput")`) — and absent for a secret the SERVICE manages, whose
//! document layout is not the caller's to read.
//!
//! The rows differ along what the rule generalises over: the direction the secret's home moves
//! in across successive updates of the one provider (managed -> managed, managed -> external,
//! external -> another external key, external -> managed), the vendor and the
//! `Oauth2ProviderConfigInput` member it selects, and the length of the key named — including
//! the 128 characters `SecretJsonKeyType`'s `@length` admits at most. The last row is what
//! pins "absent rather than stale": the provider HELD a JSON key before it, so a member left
//! over from the previous configuration would show.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a provider's client secret lives in a real
//! secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request in this suite signs for.
const REGION: &str = "eu-west-1";

/// `SecretSourceType`'s two values: a secret the SERVICE manages for the provider, and one
/// the CALLER manages in Secrets Manager (`smithy_query(query="shape",shape="SecretSourceType")`).
const SOURCE_MANAGED: &str = "MANAGED";
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The secret of the caller's that the EXTERNAL rows reference, and the two JSON keys its
/// document carries: one short, one as long as `SecretJsonKeyType`'s `@length` max of 128.
const EXTERNAL_SECRET_NAME: &str = "a-partner-oauth2-client";
const SHORT_JSON_KEY: &str = "clientSecret";

/// 128 characters: the longest key `SecretJsonKeyType` admits.
fn longest_json_key() -> String {
    format!("k{}", "e".repeat(127))
}

struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// POST one `restJson1` operation of this service, signed for [`REGION`].
    async fn control_plane(&self, uri: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("host", "bedrock-agentcore-control.amazonaws.com")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body is serializable"),
            ))
            .expect("a valid request");
        answer(self.router.dispatch(request).await)
    }

    /// POST one `awsJson1_1` Secrets Manager operation, signed for [`REGION`].
    async fn secrets_manager(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{REGION}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("secretsmanager.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/secretsmanager/aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body is serializable"),
            ))
            .expect("a valid request");
        answer(self.router.dispatch(request).await)
    }

    /// Create the secret of the CALLER's the external rows reference, holding a document
    /// with both JSON keys they name.
    async fn create_external_secret(&self) {
        let mut document = serde_json::Map::new();
        document.insert(SHORT_JSON_KEY.to_string(), json!("the-partners-secret"));
        document.insert(longest_json_key(), json!("the-partners-other-secret"));
        let value = Value::Object(document).to_string();
        let (status, created) = self
            .secrets_manager(
                "CreateSecret",
                &json!({
                    "Name": EXTERNAL_SECRET_NAME,
                    "SecretString": value,
                    // Secrets Manager stakes the version a create stores on a
                    // `ClientRequestToken` the caller names ("We recommend a UUID-type
                    // value"), so this suite's one secret carries one.
                    "ClientRequestToken": "6b0f3a1c-2d4e-4f70-9a81-5c7e3b2d1f08",
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: expected HTTP 200 from Secrets Manager CreateSecret, got {status} ({created})"
        );
    }

    async fn create(&self, body: &Value) -> (u16, Value) {
        self.control_plane("/identities/CreateOauth2CredentialProvider", body)
            .await
    }

    async fn update(&self, body: &Value) -> (u16, Value) {
        self.control_plane("/identities/UpdateOauth2CredentialProvider", body)
            .await
    }

    async fn get(&self, name: &str) -> (u16, Value) {
        self.control_plane(
            "/identities/GetOauth2CredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    /// Create a provider that must be served, answered as the create's envelope.
    async fn create_ok(&self, body: &Value, case: &str) -> Value {
        let (status, created) = self.create(body).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from the create, got {status} ({created})"
        );
        created
    }
}

fn answer(response: http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Whether `member` is present in an envelope at all — an omitted optional member is not
/// the same answer as one carrying `null`, and the rule is about the member SET.
fn carries(envelope: &Value, member: &str) -> bool {
    envelope.get(member).is_some_and(|value| !value.is_null())
}

/// The string `member` carries, as a plain string.
fn text(envelope: &Value, member: &str) -> String {
    envelope[member].as_str().unwrap_or_default().to_string()
}

/// A configuration carried in `member`, with the client secret the SERVICE is to manage.
fn managed_config(member: &str, client_id: &str) -> Value {
    json!({member: {"clientId": client_id, "clientSecret": "a-handed-over-secret"}})
}

/// A configuration carried in `member`, over a secret of the CALLER's read under `json_key`.
fn external_config(member: &str, client_id: &str, json_key: &str) -> Value {
    json!({member: {
        "clientId": client_id,
        "clientSecretSource": SOURCE_EXTERNAL,
        "clientSecretConfig": {"secretId": EXTERNAL_SECRET_NAME, "jsonKey": json_key},
    }})
}

/// One update: what it is a case of, the vendor and configuration it carries, and the
/// representation its answer must have.
struct Row {
    case: &'static str,
    vendor: &'static str,
    configuration: Value,
    /// The source the secret is expected to be reported under afterwards.
    expected_source: &'static str,
    /// The JSON key the answer must report, or `None` where it must report no member.
    expected_json_key: Option<String>,
}

/// RULE: the update's answer reports the JSON key the configuration it carried named, and no
/// `clientSecretJsonKey` member at all for a secret the service manages — before and after a
/// configuration that DID name one.
#[tokio::test]
async fn an_update_reports_the_json_key_its_configuration_named_and_omits_it_otherwise() {
    let emulator = Emulator::new();
    emulator.create_external_secret().await;
    let provider = "representation-provider";

    // The provider starts out with a secret the SERVICE manages, so it holds no JSON key
    // for an update to carry over.
    let created = emulator
        .create_ok(
            &json!({
                "name": provider,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": managed_config(
                    "googleOauth2ProviderConfig", "the-first-client",
                ),
            }),
            "the provider under test",
        )
        .await;
    assert!(
        !carries(&created, "clientSecretJsonKey"),
        "case \"the create of a managed provider\": expected no clientSecretJsonKey member, got \
         {created}"
    );

    let rows = [
        Row {
            case: "another vendor, the secret still the service's to manage",
            vendor: "GithubOauth2",
            configuration: managed_config("githubOauth2ProviderConfig", "a-github-client"),
            expected_source: SOURCE_MANAGED,
            expected_json_key: None,
        },
        Row {
            case: "the secret moves to one the caller manages, read under a short key",
            vendor: "GoogleOauth2",
            configuration: external_config(
                "googleOauth2ProviderConfig",
                "a-google-client",
                SHORT_JSON_KEY,
            ),
            expected_source: SOURCE_EXTERNAL,
            expected_json_key: Some(SHORT_JSON_KEY.to_string()),
        },
        Row {
            case: "the same secret under the longest key the model admits",
            vendor: "SlackOauth2",
            configuration: external_config(
                "slackOauth2ProviderConfig",
                "a-slack-client",
                &longest_json_key(),
            ),
            expected_source: SOURCE_EXTERNAL,
            expected_json_key: Some(longest_json_key()),
        },
        Row {
            case: "the secret moves back to one the service manages, so the key it HELD goes",
            vendor: "OktaOauth2",
            configuration: json!({"includedOauth2ProviderConfig": {
                "clientId": "an-okta-client",
                "clientSecret": "another-handed-over-secret",
                "issuer": "https://dev-3.okta.example.com",
            }}),
            expected_source: SOURCE_MANAGED,
            expected_json_key: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, updated) = emulator
            .update(&json!({
                "name": provider,
                "credentialProviderVendor": row.vendor,
                "oauth2ProviderConfigInput": row.configuration,
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the update, got {status} ({updated})"
        );
        assert_eq!(
            updated["clientSecretSource"],
            json!(row.expected_source),
            "case {case:?}: expected clientSecretSource {:?}, got {}",
            row.expected_source,
            updated["clientSecretSource"]
        );

        match &row.expected_json_key {
            Some(key) => assert_eq!(
                updated["clientSecretJsonKey"],
                json!(key),
                "case {case:?}: expected clientSecretJsonKey {key:?}, got {}",
                updated["clientSecretJsonKey"]
            ),
            None => assert!(
                !carries(&updated, "clientSecretJsonKey"),
                "case {case:?}: expected no clientSecretJsonKey member at all, got {}",
                updated["clientSecretJsonKey"]
            ),
        }

        // The update REPORTED what it wrote: the read of the provider afterwards has the
        // same member set, so neither answer is a representation of its own.
        let (status, fetched) = emulator.get(provider).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read, got {status} ({fetched})"
        );
        assert_eq!(
            (
                carries(&fetched, "clientSecretJsonKey"),
                fetched["clientSecretJsonKey"].clone()
            ),
            (
                carries(&updated, "clientSecretJsonKey"),
                updated["clientSecretJsonKey"].clone()
            ),
            "case {case:?}: the read disagrees with the update about clientSecretJsonKey, got \
             {fetched} after {updated}"
        );
    }
}

/// RULE, on the other optional member of the same envelope: every update reports the callback
/// URL the provider was published with — the address a client rebuilds its authorization
/// redirect from — rather than leaving it for a second call to fetch.
///
/// The rows are the same spread of configurations, so the member is not reported for one
/// vendor or one secret source only.
#[tokio::test]
async fn every_update_reports_the_callback_url_the_provider_was_published_with() {
    let emulator = Emulator::new();
    emulator.create_external_secret().await;
    let provider = "callback-representation";

    let created = emulator
        .create_ok(
            &json!({
                "name": provider,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": managed_config(
                    "googleOauth2ProviderConfig", "the-first-client",
                ),
            }),
            "the provider under test",
        )
        .await;
    let published = text(&created, "callbackUrl");
    assert!(
        published.starts_with("http"),
        "case \"the create\": expected a callback URL, got {published:?} in {created}"
    );

    // (case, vendor, the configuration the update carries)
    let rows: [(&str, &str, Value); 3] = [
        (
            "an update keeping the secret the service manages",
            "GithubOauth2",
            managed_config("githubOauth2ProviderConfig", "a-github-client"),
        ),
        (
            "an update over a secret the caller manages",
            "SlackOauth2",
            external_config("slackOauth2ProviderConfig", "a-slack-client", SHORT_JSON_KEY),
        ),
        (
            "an update onto the included member of another vendor",
            "OktaOauth2",
            json!({"includedOauth2ProviderConfig": {
                "clientId": "an-okta-client",
                "clientSecret": "another-handed-over-secret",
                "issuer": "https://dev-4.okta.example.com",
            }}),
        ),
    ];

    for (case, vendor, configuration) in rows {
        let (status, updated) = emulator
            .update(&json!({
                "name": provider,
                "credentialProviderVendor": vendor,
                "oauth2ProviderConfigInput": configuration,
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the update, got {status} ({updated})"
        );
        assert_eq!(
            text(&updated, "callbackUrl"),
            published,
            "case {case:?}: expected the update to report the published callback {published:?}, \
             got {}",
            text(&updated, "callbackUrl")
        );
        let (status, fetched) = emulator.get(provider).await;
        assert_eq!(
            (status, text(&fetched, "callbackUrl")),
            (200, published.clone()),
            "case {case:?}: expected the read after the update to report {published:?}, got \
             {fetched}"
        );
    }
}
