// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the two operations that MUTATE an OAuth2 credential provider
//! this account already holds: `UpdateOauth2CredentialProvider` and
//! `DeleteOauth2CredentialProvider`.
//!
//! The update's rule under test: an update REPLACES the whole configuration of a provider this
//! account already holds — the vendor, the `Oauth2ProviderConfigInput` member that vendor
//! selects and the secret the client secret it authenticates with lives in are all
//! `@required` on the request (`smithy_query(query="operation",
//! operation="UpdateOauth2CredentialProvider")`) — while everything that IDENTIFIES the
//! provider rather than configures it survives: the name it is addressed by, the ARN
//! minted from that name and the instant it was created. The update's own answer is what
//! a later read reports, and `lastUpdatedTime` never goes backwards.
//!
//! Its two refusal classes follow from the same rule: a `name` resolving to no provider of
//! this account is the modelled HTTP 404 (an update configures an existing provider, it
//! never mints one), and a vendor and a configuration member that disagree are the modelled
//! HTTP 400 — in both cases the account is left exactly as it was.
//!
//! The delete's rule under test, read off the same model
//! (`smithy_query(query="operation",operation="DeleteOauth2CredentialProvider,\
//! GetOauth2CredentialProvider,ListOauth2CredentialProviders")`): a delete of a resource
//! this account holds RETIRES it, so the resource stops resolving. It answers HTTP 204
//! carrying no member and its answer reports no status, so the removal is immediate: every
//! later read of that `name` is the modelled `ResourceNotFoundException` (HTTP 404), worded
//! as the read route words that same missing name, the listing stops reporting it, the name
//! is free for a new provider, and the delete retires the ONE provider it names — the
//! account's other providers, and the Secrets Manager secret behind each provider's client
//! secret, are left exactly as they were. Its rows differ along what the rule generalises
//! over: the vendor and the `Oauth2ProviderConfigInput` member it selects, where the client
//! secret lives (`MANAGED`, `EXTERNAL`), the shape and length of the provider's name, and
//! whether the request carried tags.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a provider's client secret is stored in
//! a real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request in this suite signs for.
const REGION: &str = "us-east-2";

/// `SecretSourceType`'s two values: a secret the SERVICE manages for the provider, and one
/// the CALLER manages in Secrets Manager (`smithy_query shape=SecretSourceType`).
const SOURCE_MANAGED: &str = "MANAGED";
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The two documents an authorization server's metadata is published as — OpenID Connect
/// Discovery 1.0 and RFC 8414 — which are the only endings `Oauth2Discovery$discoveryUrl`'s
/// modelled pattern admits (`smithy_query shape=Oauth2Discovery`).
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";
const OAUTH_AUTHORIZATION_SERVER: &str = "/.well-known/oauth-authorization-server";

/// `Status`'s value for a provider whose secret is stored or resolved, which is what an
/// update that answered has already done (`smithy_query shape=Status`).
const STATUS_READY: &str = "READY";

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

    /// Create a secret of the CALLER's holding `value`, answered as the ARN Secrets
    /// Manager minted for it.
    async fn create_secret(&self, name: &str, value: &str) -> String {
        let (status, created) = self
            .secrets_manager(
                "CreateSecret",
                &json!({
                    "Name": name,
                    "SecretString": value,
                    // Secrets Manager stakes the version a create stores on a
                    // `ClientRequestToken` the caller names ("We recommend a UUID-type
                    // value"), so this suite's one secret carries one.
                    "ClientRequestToken": "4f1c0a3e-9b7d-4c21-8a55-0d6e2f7b1c94",
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "expected HTTP 200 from Secrets Manager CreateSecret for {name:?}, got {status} \
             ({created})"
        );
        created["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateSecret reported no ARN for {name:?} in {created}"))
            .to_string()
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

    async fn delete(&self, name: &str) -> (u16, Value) {
        self.control_plane(
            "/identities/DeleteOauth2CredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    /// What `GetSecretValue` answers for `secret_id`, which is how a row observes that the
    /// secret behind a provider's client secret is still the one Secrets Manager holds.
    async fn get_secret_value(&self, secret_id: &str) -> (u16, Value) {
        self.secrets_manager("GetSecretValue", &json!({"SecretId": secret_id}))
            .await
    }

    async fn list(&self) -> (u16, Value) {
        self.control_plane("/identities/ListOauth2CredentialProviders", &json!({}))
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

/// A configuration carried in `member`, with the three members every built-in vendor's
/// configuration declares.
fn vendor_config(member: &str, client_id: &str, client_secret: &str) -> Value {
    json!({member: {"clientId": client_id, "clientSecret": client_secret}})
}

/// The token vault a request naming none addresses, which a managed secret's name carries.
const VAULT: &str = "default";

/// The name the secret holding a managed provider's client secret is created under. Spelled
/// out here rather than read off the implementation, so a drift in either is a failing test.
fn managed_secret_name(provider: &str) -> String {
    format!("bedrock-agentcore-identity/{VAULT}/oauth2/{provider}")
}

/// The `message` an answer carries, as a plain string.
///
/// A delete's refusal of a name no provider resolves to is asserted against the READ
/// route's refusal of that same name rather than against a sentence spelled out here: a
/// missing provider is ONE fact of this plane, so the two routes word it the same way,
/// whatever that wording is — and how this plane words it is the subject of its own open
/// report, which pinning the sentence here would pin the wrong side of.
fn message(envelope: &Value) -> String {
    envelope["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The single member an `Oauth2ProviderConfigOutput` union carries, and its name.
fn reported_config(envelope: &Value) -> (String, Value) {
    envelope["oauth2ProviderConfigOutput"]
        .as_object()
        .and_then(|members| {
            members
                .iter()
                .next()
                .map(|(member, config)| (member.clone(), config.clone()))
        })
        .unwrap_or_default()
}

/// The timestamp `member` carries, as the epoch seconds `restJson1` renders a `Timestamp`
/// as.
fn instant(envelope: &Value, member: &str) -> f64 {
    envelope[member].as_f64().unwrap_or(f64::NAN)
}

/// The names a listing reports.
fn listed_names(body: &Value) -> Vec<String> {
    body["credentialProviders"]
        .as_array()
        .map(|providers| {
            providers
                .iter()
                .map(|provider| provider["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Successive updates of ONE provider each replace its whole configuration, keep the
/// identity it was created with, and are what the read reports afterwards.
///
/// The rows differ along the shape of the configuration the update supplies: another
/// built-in vendor's member, the custom member with an authorization server of the
/// caller's, the same vendor again with a different value in it (the directory a Microsoft
/// provider is scoped to), the included member of a vendor the union declares no member of
/// its own for, and a configuration whose client secret is one the CALLER manages — so the
/// rule under test is "the configuration the update carried is the provider's
/// configuration", not any one vendor's.
#[tokio::test]
async fn an_update_replaces_the_configuration_and_keeps_the_providers_identity() {
    let emulator = Emulator::new();
    let provider = "lifecycle-provider";

    let created = emulator
        .create_ok(
            &json!({
                "name": provider,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "googleOauth2ProviderConfig", "the-first-client", "the-first-secret",
                ),
                "tags": {"owner": "identity"},
            }),
            "the provider under test",
        )
        .await;
    let arn = created["credentialProviderArn"].clone();
    let managed_secret_arn = created["clientSecretArn"]["secretArn"].clone();
    let (_, before_any_update) = emulator.get(provider).await;
    let created_time = instant(&before_any_update, "createdTime");

    // A secret of the caller's for the EXTERNAL row, created through Secrets Manager's own
    // operation so the provider is updated against a secret that really exists.
    let external_secret_arn = emulator
        .create_secret(
            "a-partner-oauth2-client",
            &json!({"clientSecret": "the-partners-secret"}).to_string(),
        )
        .await;

    // (case, vendor, the configuration supplied, the member expected back, the discovery
    //  expected back, the secret source expected back)
    let rows: [(&str, &str, Value, &str, String, &str); 5] = [
        (
            "another built-in vendor, whose own metadata document is reported",
            "GithubOauth2",
            vendor_config(
                "githubOauth2ProviderConfig",
                "a-github-client",
                "a-github-secret",
            ),
            "githubOauth2ProviderConfig",
            // GitHub serves OAuth2 without OpenID Connect, so RFC 8414's document.
            format!("https://github.com{OAUTH_AUTHORIZATION_SERVER}"),
            SOURCE_MANAGED,
        ),
        (
            "the custom member, with an authorization server of the caller's",
            "CustomOauth2",
            json!({"customOauth2ProviderConfig": {
                "oauthDiscovery": {"discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}")},
                "clientId": "a-custom-client",
                "clientSecret": "a-custom-secret",
            }}),
            "customOauth2ProviderConfig",
            format!("https://auth.example.com{OPENID_CONFIGURATION}"),
            SOURCE_MANAGED,
        ),
        (
            "a vendor whose server is scoped to the directory the request names",
            "MicrosoftOauth2",
            json!({"microsoftOauth2ProviderConfig": {
                "clientId": "a-microsoft-client",
                "clientSecret": "a-microsoft-secret",
                "tenantId": "00000000-0000-0000-0000-00000000000a",
            }}),
            "microsoftOauth2ProviderConfig",
            format!(
                "https://login.microsoftonline.com/00000000-0000-0000-0000-00000000000a/v2.0{OPENID_CONFIGURATION}"
            ),
            SOURCE_MANAGED,
        ),
        (
            "the same vendor again with another directory, which is another server",
            "MicrosoftOauth2",
            json!({"microsoftOauth2ProviderConfig": {
                "clientId": "a-microsoft-client",
                "clientSecret": "a-microsoft-secret",
                "tenantId": "00000000-0000-0000-0000-00000000000b",
            }}),
            "microsoftOauth2ProviderConfig",
            format!(
                "https://login.microsoftonline.com/00000000-0000-0000-0000-00000000000b/v2.0{OPENID_CONFIGURATION}"
            ),
            SOURCE_MANAGED,
        ),
        (
            "the included member of a vendor the union declares no member for, over a \
             secret the caller manages",
            "OktaOauth2",
            json!({"includedOauth2ProviderConfig": {
                "clientId": "an-okta-client",
                "issuer": "https://dev-7.okta.example.com/",
                "clientSecretSource": SOURCE_EXTERNAL,
                "clientSecretConfig": {
                    "secretId": "a-partner-oauth2-client",
                    "jsonKey": "clientSecret",
                },
            }}),
            "includedOauth2ProviderConfig",
            format!("https://dev-7.okta.example.com{OPENID_CONFIGURATION}"),
            SOURCE_EXTERNAL,
        ),
    ];

    let mut previous_update = created_time;
    for (case, vendor, configuration, expected_member, expected_discovery, expected_source) in rows
    {
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

        // The configuration the update carried is the provider's configuration: the member
        // it supplied, the authorization server that configuration names, and nothing of
        // the configuration it replaced.
        let (member, config) = reported_config(&updated);
        assert_eq!(
            (member.as_str(), config["oauthDiscovery"]["discoveryUrl"].clone()),
            (expected_member, json!(expected_discovery)),
            "case {case:?}: the update reports the configuration it was given, got {updated}"
        );
        assert_eq!(
            (
                updated["credentialProviderVendor"].clone(),
                updated["clientSecretSource"].clone(),
                updated["status"].clone()
            ),
            (json!(vendor), json!(expected_source), json!(STATUS_READY)),
            "case {case:?}: the update reports the vendor and the secret source it was \
             given, got {updated}"
        );
        // The client secret the caller handed over is held in a secret rather than echoed.
        assert!(
            !updated.to_string().contains("-secret\""),
            "case {case:?}: the update echoed a client secret, in {updated}"
        );
        let expected_secret_arn = if expected_source == SOURCE_EXTERNAL {
            json!(external_secret_arn)
        } else {
            // A managed secret is named for the provider, so every managed update of one
            // provider puts its value into the same secret.
            managed_secret_arn.clone()
        };
        assert_eq!(
            updated["clientSecretArn"]["secretArn"], expected_secret_arn,
            "case {case:?}: the update reports the secret the client secret lives in, got \
             {updated}"
        );

        // What identifies the provider survives the update, and the clock only moves
        // forward.
        assert_eq!(
            (
                updated["name"].clone(),
                updated["credentialProviderArn"].clone(),
                instant(&updated, "createdTime")
            ),
            (json!(provider), arn.clone(), created_time),
            "case {case:?}: the update changed what identifies the provider, got {updated}"
        );
        let last_updated = instant(&updated, "lastUpdatedTime");
        assert!(
            last_updated >= previous_update,
            "case {case:?}: lastUpdatedTime went backwards, expected at least \
             {previous_update}, got {last_updated}"
        );
        previous_update = last_updated;

        // The update WROTE what it reported: the read answers with the same provider, and
        // the account still holds exactly one of it.
        let (status, fetched) = emulator.get(provider).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read, got {status} ({fetched})"
        );
        for member in [
            "credentialProviderVendor",
            "oauth2ProviderConfigOutput",
            "clientSecretArn",
            "clientSecretSource",
            "clientSecretJsonKey",
            "credentialProviderArn",
            "name",
            "createdTime",
            "lastUpdatedTime",
            "status",
        ] {
            assert_eq!(
                fetched[member], updated[member],
                "case {case:?}: the read disagrees with the update about {member:?}, got \
                 {fetched}"
            );
        }
        assert_eq!(
            listed_names(&emulator.list().await.1),
            vec![provider.to_string()],
            "case {case:?}: the account holds exactly the one updated provider"
        );
    }
}

/// An update of a `name` this account holds no provider under is the modelled 404, and
/// mints nothing: an update configures an existing provider.
///
/// The rows differ along why the name resolves to nothing — never used at all, the name of
/// a provider in another letter case, and a name whose only create was refused.
#[tokio::test]
async fn an_update_of_a_name_no_provider_resolves_to_is_refused() {
    let emulator = Emulator::new();
    emulator
        .create_ok(
            &json!({
                "name": "held-provider",
                "credentialProviderVendor": "SlackOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "slackOauth2ProviderConfig", "a-client", "a-secret",
                ),
            }),
            "the one provider this account holds",
        )
        .await;

    // A create whose vendor and configuration disagree leaves no provider behind, so this
    // name is one no provider stands under either.
    let (status, _) = emulator
        .create(&json!({
            "name": "refused-provider",
            "credentialProviderVendor": "GoogleOauth2",
            "oauth2ProviderConfigInput": vendor_config(
                "githubOauth2ProviderConfig", "a-client", "a-secret",
            ),
        }))
        .await;
    assert_eq!(
        status, 400,
        "the setup create is refused, so its name holds no provider"
    );

    // (case, the name the update addresses)
    let rows: [(&str, &str); 3] = [
        ("a name never used", "no-such-provider"),
        (
            "the held provider's name in another letter case",
            "Held-Provider",
        ),
        ("a name whose only create was refused", "refused-provider"),
    ];

    for (case, name) in rows {
        let (status, refused) = emulator
            .update(&json!({
                "name": name,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "googleOauth2ProviderConfig", "a-client", "a-secret",
                ),
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {status} \
             ({refused})"
        );
        assert_eq!(
            refused["message"].as_str().unwrap_or_default(),
            // The OAuth2 credential-provider family's own fixed sentence: it names
            // neither the provider kind nor the name the request addressed
            // (`tests/all/credential_provider_type_isolation.rs`).
            "CredentialProvider not found",
            "case {case:?}: unexpected refusal message, got {refused}"
        );

        let (status, fetched) = emulator.get(name).await;
        assert_eq!(
            (status, error_code(&fetched).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: the refused update minted a provider, got {status} ({fetched})"
        );
        assert_eq!(
            listed_names(&emulator.list().await.1),
            vec!["held-provider".to_string()],
            "case {case:?}: the refused update changed what the account holds"
        );
    }
}

/// An update whose vendor and configuration member disagree is the modelled 400, and the
/// provider is left exactly as it was.
///
/// The rows differ along which pair disagrees: a vendor with a member of its own given
/// another vendor's, a vendor with one given the custom member, the custom vendor given a
/// built-in member, and a vendor the union declares no member of its own for given one.
#[tokio::test]
async fn an_update_whose_vendor_and_configuration_disagree_leaves_the_provider_unchanged() {
    let emulator = Emulator::new();
    let provider = "unchanged-provider";
    emulator
        .create_ok(
            &json!({
                "name": provider,
                "credentialProviderVendor": "AtlassianOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "atlassianOauth2ProviderConfig", "a-client", "a-secret",
                ),
            }),
            "the provider under test",
        )
        .await;
    let (_, before) = emulator.get(provider).await;

    // (case, the vendor declared, the configuration supplied)
    let rows: [(&str, &str, Value); 4] = [
        (
            "a vendor given another vendor's member",
            "GoogleOauth2",
            vendor_config("linkedinOauth2ProviderConfig", "a-client", "a-secret"),
        ),
        (
            "a vendor given the custom member",
            "SalesforceOauth2",
            json!({"customOauth2ProviderConfig": {
                "oauthDiscovery": {"discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}")},
                "clientId": "a-client",
                "clientSecret": "a-secret",
            }}),
        ),
        (
            "the custom vendor given a built-in member",
            "CustomOauth2",
            vendor_config("slackOauth2ProviderConfig", "a-client", "a-secret"),
        ),
        (
            "a vendor the union declares no member for, given a named vendor's member",
            "NotionOauth2",
            vendor_config("googleOauth2ProviderConfig", "a-client", "a-secret"),
        ),
    ];

    for (case, vendor, configuration) in rows {
        let (status, refused) = emulator
            .update(&json!({
                "name": provider,
                "credentialProviderVendor": vendor,
                "oauth2ProviderConfigInput": configuration,
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} ({refused})"
        );
        assert_eq!(
            refused["message"].as_str().unwrap_or_default(),
            format!("Provided configuration does not match selected type: {vendor}"),
            "case {case:?}: unexpected refusal message, got {refused}"
        );

        let (status, after) = emulator.get(provider).await;
        assert_eq!(
            (status, &after),
            (200, &before),
            "case {case:?}: the refused update changed the provider, got {status} ({after})"
        );
    }
}

/// A `discoveryUrl` outside its modelled pattern refuses the update in this service's
/// constraint-violation wording, and the provider keeps the server it had.
#[tokio::test]
async fn an_update_with_a_discovery_url_outside_its_pattern_is_refused() {
    let emulator = Emulator::new();
    let provider = "pattern-provider";
    let served = format!("https://first.example.com{OPENID_CONFIGURATION}");
    emulator
        .create_ok(
            &json!({
                "name": provider,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": {"customOauth2ProviderConfig": {
                    "oauthDiscovery": {"discoveryUrl": served},
                    "clientId": "a-client",
                    "clientSecret": "a-secret",
                }},
            }),
            "the provider under test",
        )
        .await;

    // Each way the pattern can fail: no well-known path, a document it does not admit, no
    // issuer before the document, and the delimiters that end a URL's path — the query and
    // the fragment — which the character class the issuer and any trailing segment are
    // drawn from excludes.
    let rows: [(&str, String); 5] = [
        (
            "no well-known path at all",
            "https://second.example.com/oauth".to_string(),
        ),
        (
            "a well-known document the pattern does not admit",
            "https://second.example.com/.well-known/jwks.json".to_string(),
        ),
        (
            "the metadata document with no issuer before it",
            OPENID_CONFIGURATION.to_string(),
        ),
        (
            "the metadata document carrying a query string",
            format!("https://second.example.com{OPENID_CONFIGURATION}?tenant=t1"),
        ),
        (
            "the metadata document carrying a fragment",
            format!("https://second.example.com{OPENID_CONFIGURATION}#section"),
        ),
    ];

    for (case, url) in rows {
        let (status, refused) = emulator
            .update(&json!({
                "name": provider,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": {"customOauth2ProviderConfig": {
                    "oauthDiscovery": {"discoveryUrl": url},
                    "clientId": "another-client",
                    "clientSecret": "another-secret",
                }},
            }))
            .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} ({refused})"
        );

        let (_, fetched) = emulator.get(provider).await;
        assert_eq!(
            reported_config(&fetched).1["oauthDiscovery"]["discoveryUrl"],
            json!(served),
            "case {case:?}: the refused update changed the provider's authorization \
             server, got {fetched}"
        );
    }
}

/// One row of the deletion table: the provider a delete retires, and how it was created.
struct DeletionRow {
    case: &'static str,
    /// The provider's name — the identifier every provider route addresses it by.
    name: String,
    vendor: &'static str,
    /// The `Oauth2ProviderConfigInput` the row's vendor selects.
    configuration: Value,
    /// A secret of the CALLER's own this row references, created before the provider is.
    external_secret: Option<(&'static str, &'static str)>,
    tags: Option<Value>,
}

/// The longest name `CredentialProviderName` admits: length 1..128, pattern
/// `^[a-zA-Z0-9\-_]+$` (`smithy_query(query="shape",shape="CredentialProviderName")`).
const MAX_PROVIDER_NAME_LEN: usize = 128;

fn deletion_rows() -> Vec<DeletionRow> {
    vec![
        DeletionRow {
            case: "a built-in vendor's own member, over a secret the service manages",
            name: "orders-oauth2".to_string(),
            vendor: "GoogleOauth2",
            configuration: vendor_config(
                "googleOauth2ProviderConfig",
                "an-orders-client",
                "an-orders-secret",
            ),
            external_secret: None,
            tags: None,
        },
        DeletionRow {
            case: "the shortest name the pattern admits, with tags",
            name: "a".to_string(),
            vendor: "SlackOauth2",
            configuration: vendor_config("slackOauth2ProviderConfig", "a", "s"),
            external_secret: None,
            tags: Some(json!({"team": "identity"})),
        },
        DeletionRow {
            case: "the custom member, with an authorization server of the caller's, under \
                   the longest name the pattern admits",
            name: format!("{:_>width$}", "Z", width = MAX_PROVIDER_NAME_LEN),
            vendor: "CustomOauth2",
            configuration: json!({"customOauth2ProviderConfig": {
                "oauthDiscovery": {
                    "discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}"),
                },
                "clientId": "a-custom-client",
                "clientSecret": "a-custom-secret",
            }}),
            external_secret: None,
            tags: None,
        },
        DeletionRow {
            case: "the included member of a vendor the union declares no member for, whose \
                   client secret is the caller's own",
            name: "search_OAuth2-provider_2".to_string(),
            vendor: "OktaOauth2",
            configuration: json!({"includedOauth2ProviderConfig": {
                "clientId": "an-okta-client",
                "issuer": "https://dev-7.okta.example.com/",
                "clientSecretSource": SOURCE_EXTERNAL,
                "clientSecretConfig": {
                    "secretId": "my/own/okta-client",
                    "jsonKey": "clientSecret",
                },
            }}),
            external_secret: Some((
                "my/own/okta-client",
                r#"{"clientSecret":"the-partners-secret"}"#,
            )),
            tags: Some(json!({"owner": "search"})),
        },
    ]
}

impl DeletionRow {
    /// The `CreateOauth2CredentialProvider` body this row sends, once any secret of the
    /// caller's own it references has been created.
    async fn create_body(&self, emulator: &Emulator) -> Value {
        if let Some((secret_name, secret_value)) = self.external_secret {
            emulator.create_secret(secret_name, secret_value).await;
        }
        let mut body = json!({
            "name": self.name,
            "credentialProviderVendor": self.vendor,
            "oauth2ProviderConfigInput": self.configuration,
        });
        if let Some(tags) = &self.tags {
            body["tags"] = tags.clone();
        }
        body
    }
}

/// A deleted provider stops resolving at once: the delete answers 204 carrying no member,
/// every later read of that name is the modelled 404 naming it, the listing stops reporting
/// it, a repeat of the delete is that same 404, and the name is free for a new provider.
#[tokio::test]
async fn a_deleted_oauth2_credential_provider_stops_resolving() {
    for row in deletion_rows() {
        let emulator = Emulator::new();
        let body = row.create_body(&emulator).await;
        emulator.create_ok(&body, row.case).await;
        let (status, readable) = emulator.get(&row.name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the created provider is readable before the delete, got {status} \
             ({readable})",
            row.case
        );
        assert_eq!(
            listed_names(&emulator.list().await.1),
            vec![row.name.clone()],
            "case {:?}: the account holds the created provider before the delete",
            row.case
        );

        let (status, deleted) = emulator.delete(&row.name).await;
        let carries_no_member = match &deleted {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        };
        assert!(
            status == 204 && carries_no_member,
            "case {:?}: the delete answers 204 carrying no member, got {status} ({deleted})",
            row.case
        );

        let (status, gone) = emulator.get(&row.name).await;
        assert_eq!(
            (status, error_code(&gone)),
            (404, "ResourceNotFoundException".to_string()),
            "case {:?}: a deleted provider is the modelled 404, got {status} ({gone})",
            row.case
        );
        // The sentence the read route words a missing provider with, which the delete of
        // the same missing name has to word it with too.
        let missing = message(&gone);
        assert!(
            !missing.is_empty(),
            "case {:?}: the refusal of a deleted provider carries no message, got {gone}",
            row.case
        );
        assert!(
            listed_names(&emulator.list().await.1).is_empty(),
            "case {:?}: the listing still reports the deleted provider",
            row.case
        );
        // And it stays gone: a read again answers the same way, rather than the resource
        // coming back.
        let (status, still_gone) = emulator.get(&row.name).await;
        assert_eq!(
            (status, message(&still_gone)),
            (404, missing.clone()),
            "case {:?}: a retired provider does not come back, got {status} ({still_gone})",
            row.case
        );

        let (status, repeated) = emulator.delete(&row.name).await;
        assert_eq!(
            (status, error_code(&repeated), message(&repeated)),
            (
                404,
                "ResourceNotFoundException".to_string(),
                missing.clone()
            ),
            "case {:?}: a delete of a retired provider is the modelled 404, worded as the \
             read words that same missing name, got {status} ({repeated})",
            row.case
        );

        // "The name must be unique within your account" held the name while the provider
        // existed; a deleted provider holds it no longer.
        let recreated = emulator.create_ok(&body, row.case).await;
        let (status, read_back) = emulator.get(&row.name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the re-created provider is readable, got {status} ({read_back})",
            row.case
        );
        assert_eq!(
            read_back["credentialProviderArn"], recreated["credentialProviderArn"],
            "case {:?}: the read disagrees with the re-create about the provider's ARN, got \
             {read_back}",
            row.case
        );
    }
}

/// A delete retires the ONE provider it names: the account's other providers stay readable,
/// and the secret a provider's client secret lives in — the caller's own for an `EXTERNAL`
/// provider, the one the service manages for a `MANAGED` one — is left exactly as it was,
/// so a later create under the retired name reports the client secret its own create stored.
#[tokio::test]
async fn deleting_a_provider_leaves_the_other_providers_and_their_secrets_alone() {
    let emulator = Emulator::new();

    // A managed provider: the create put the client secret into a secret of the service's.
    emulator
        .create_ok(
            &json!({
                "name": "managed-oauth2",
                "credentialProviderVendor": "GithubOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "githubOauth2ProviderConfig", "a-github-client", "the-first-secret",
                ),
            }),
            "the managed provider under test",
        )
        .await;
    let managed_secret = managed_secret_name("managed-oauth2");
    let (status, before) = emulator.get_secret_value(&managed_secret).await;
    assert_eq!(
        status, 200,
        "setup: the managed secret holds the client secret before the delete, got {status} \
         ({before})"
    );

    // An external provider beside it: its secret is the caller's own.
    let external_secret = "my/own/retained-oauth2-client";
    emulator
        .create_secret(
            external_secret,
            &json!({"clientSecret": "the-callers-secret"}).to_string(),
        )
        .await;
    emulator
        .create_ok(
            &json!({
                "name": "external-oauth2",
                "credentialProviderVendor": "OktaOauth2",
                "oauth2ProviderConfigInput": {"includedOauth2ProviderConfig": {
                    "clientId": "an-okta-client",
                    "issuer": "https://dev-9.okta.example.com/",
                    "clientSecretSource": SOURCE_EXTERNAL,
                    "clientSecretConfig": {
                        "secretId": external_secret,
                        "jsonKey": "clientSecret",
                    },
                }},
            }),
            "the external provider beside it",
        )
        .await;

    let (status, deleted) = emulator.delete("managed-oauth2").await;
    assert_eq!(
        status, 204,
        "the managed provider's delete is served, got {status} ({deleted})"
    );
    let (status, gone) = emulator.get("managed-oauth2").await;
    assert_eq!(
        (status, error_code(&gone)),
        (404, "ResourceNotFoundException".to_string()),
        "the deleted provider resolves nothing, got {status} ({gone})"
    );

    // The other provider is untouched by that delete, and so is the secret it references.
    let (status, other) = emulator.get("external-oauth2").await;
    assert_eq!(
        status, 200,
        "deleting one provider leaves the account's other provider readable, got {status} \
         ({other})"
    );
    assert_eq!(
        listed_names(&emulator.list().await.1),
        vec!["external-oauth2".to_string()],
        "the listing reports exactly the provider the delete did not name"
    );
    let (status, caller_secret) = emulator.get_secret_value(external_secret).await;
    assert_eq!(
        status, 200,
        "the secret an external provider references is the caller's own and is left alone, \
         got {status} ({caller_secret})"
    );

    // A provider created again under the retired name reports the client secret ITS create
    // stored, which is what makes the name reusable rather than haunted by the old
    // provider's secret.
    emulator
        .create_ok(
            &json!({
                "name": "managed-oauth2",
                "credentialProviderVendor": "LinkedinOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "linkedinOauth2ProviderConfig", "a-linkedin-client", "the-second-secret",
                ),
            }),
            "the provider created again under the retired name",
        )
        .await;
    let (status, value) = emulator.get_secret_value(&managed_secret).await;
    assert_eq!(
        status, 200,
        "the new provider's client secret is readable from the secret the service manages, \
         got {status} ({value})"
    );
    assert_eq!(
        value["SecretString"].as_str().unwrap_or_default(),
        r#"{"clientSecret":"the-second-secret"}"#,
        "the secret behind the re-created provider carries ITS client secret, got {value}"
    );

    let (status, external_deleted) = emulator.delete("external-oauth2").await;
    assert_eq!(
        status, 204,
        "the external provider's delete is served, got {status} ({external_deleted})"
    );
    let (status, retained) = emulator.get_secret_value(external_secret).await;
    assert_eq!(
        status, 200,
        "deleting an external provider does not delete the caller's secret, got {status} \
         ({retained})"
    );
}

/// A delete naming a provider this account does not hold is refused the modelled 404 —
/// worded as the read route words that same missing name — and deletes nothing.
#[tokio::test]
async fn a_provider_delete_that_resolves_nothing_is_refused_and_deletes_nothing() {
    let emulator = Emulator::new();
    emulator
        .create_ok(
            &json!({
                "name": "bystander-oauth2",
                "credentialProviderVendor": "SalesforceOauth2",
                "oauth2ProviderConfigInput": vendor_config(
                    "salesforceOauth2ProviderConfig", "a-client", "a-secret",
                ),
            }),
            "the one provider this account holds",
        )
        .await;

    // Every name below satisfies `CredentialProviderName` (`^[a-zA-Z0-9\-_]+$`, 1..128), so
    // the refusal is the handler's lookup and not the boundary constraint walk: the
    // shortest name the pattern admits, one carrying both characters its pattern allows
    // beside letters and digits, one at the top of the length range, and one that differs
    // from the stored provider's name only in case.
    let unknown = [
        "x".to_string(),
        "no_such-provider".to_string(),
        format!("{:_>width$}", "Q", width = MAX_PROVIDER_NAME_LEN),
        "Bystander-Oauth2".to_string(),
    ];
    for name in unknown {
        let (_, read) = emulator.get(&name).await;
        let refusal = message(&read);
        assert!(
            !refusal.is_empty(),
            "case {name:?}: the read's refusal of an unheld name carries no message, got {read}"
        );
        let (status, refused) = emulator.delete(&name).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (404, "ResourceNotFoundException".to_string(), refusal),
            "case {name:?}: the delete refuses an unheld name as the read refuses it, got \
             {status} ({refused})"
        );
    }

    let (status, intact) = emulator.get("bystander-oauth2").await;
    assert_eq!(
        status, 200,
        "a refused delete leaves the account's provider readable, got {status} ({intact})"
    );
    assert_eq!(
        listed_names(&emulator.list().await.1),
        vec!["bystander-oauth2".to_string()],
        "a refused delete changed what the account holds"
    );
    let (status, secret) = emulator
        .get_secret_value(&managed_secret_name("bystander-oauth2"))
        .await;
    assert_eq!(
        status, 200,
        "a refused delete leaves the secret behind the provider alone, got {status} ({secret})"
    );
}
