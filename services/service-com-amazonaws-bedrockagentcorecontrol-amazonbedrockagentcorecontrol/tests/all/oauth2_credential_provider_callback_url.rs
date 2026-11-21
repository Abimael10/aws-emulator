// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the OAuth2 authorization CALLBACK address an OAuth2 credential
//! provider publishes.
//!
//! RULE: every response of this plane that declares `callbackUrl` — the create, every read
//! and the update (`smithy_query(query="operation",
//! operation="CreateOauth2CredentialProvider,GetOauth2CredentialProvider,\
//! UpdateOauth2CredentialProvider")`) — reports the address an OAuth2 authorization server
//! redirects an authorizing user back to, and that address is the OAuth2 callback of the
//! endpoint THIS emulator is served at: a client here resolves no host AWS answers on, so
//! the published value is the served base followed by this plane's callback path. It belongs
//! to the identity plane that completes the flow rather than to one provider, so it is the
//! same for every provider of the account, it is stable across reads, and an update — which
//! replaces the whole configuration — leaves it as it was.
//!
//! The first table's rows span what the rule generalises over on the PROVIDER side: the
//! vendor and the `Oauth2ProviderConfigInput` member it selects (a built-in vendor, the
//! custom member naming its own authorization server, the included member naming only an
//! issuer), who manages the client secret (`MANAGED`, `EXTERNAL`), the shape and length of
//! the provider's name, and the Region the request is signed for. The second table's rows
//! span what it generalises over on the ENDPOINT side: the base this emulator reports as its
//! endpoint — unserved, a loopback authority with a port, a named host with a port, an IPv6
//! literal and a base written with a trailing separator.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a provider's client secret is stored in
//! a real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// `SecretSourceType`'s value for a secret the CALLER manages in Secrets Manager, as opposed
/// to one the service manages for the provider (`smithy_query(query="shape",
/// shape="SecretSourceType")`).
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The published location of an authorization server's metadata document, one of the two
/// endings `Oauth2Discovery$discoveryUrl`'s modelled pattern admits.
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

/// The client secret value a managed row hands over.
const HANDED_OVER_SECRET: &str = "the-client-secret-value";

/// The path this plane receives an OAuth2 authorization redirect at, spelled out here rather
/// than read off the service, so a drift in either is a failing test: the `/identities`
/// prefix every credential-provider route of this service is served under (`smithy_query
/// operation=GetOauth2CredentialProvider` reports the URI
/// `/identities/GetOauth2CredentialProvider`), with the OAuth2 flow's callback beneath it.
const CALLBACK_PATH: &str = "/identities/oauth2/callback";

/// The callback address an emulator served at `base` publishes.
fn expected_callback_url(base: &str) -> String {
    format!("{}{CALLBACK_PATH}", base.trim_end_matches('/'))
}

/// This service and Secrets Manager over one shared engine, behind the same
/// `MultiServiceRouter` the production router builds.
struct Emulator {
    router: MultiServiceRouter,
    /// The endpoint this emulator reports it is served at, which is what a published
    /// callback address has to name.
    served_base: String,
}

impl Emulator {
    fn new() -> Self {
        Self::served_at(None)
    }

    /// The same emulator, reporting `base` as the endpoint it is served at.
    fn served_at(base: Option<&str>) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        let served_base = match base {
            Some(base) => {
                let parsed = smithy_server_runtime::EndpointUrl::parse(base)
                    .expect("a base URL the router accepts");
                let reported = parsed.as_str().to_string();
                router.set_url_base(parsed);
                reported
            }
            None => smithy_server_runtime::UNSERVED_URL_BASE.to_string(),
        };
        Self {
            router,
            served_base,
        }
    }

    /// POST one `restJson1` operation of this service, signed for `region`.
    async fn control_plane(&self, uri: &str, region: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/bedrock-agentcore/\
             aws4_request, SignedHeaders=host, Signature=deadbeef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("host", "bedrock-agentcore-control.amazonaws.com")
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body is serializable"),
            ))
            .expect("a valid request");
        answer(self.router.dispatch(request).await)
    }

    async fn create(&self, region: &str, body: &Value) -> (u16, Value) {
        self.control_plane("/identities/CreateOauth2CredentialProvider", region, body)
            .await
    }

    async fn update(&self, region: &str, body: &Value) -> (u16, Value) {
        self.control_plane("/identities/UpdateOauth2CredentialProvider", region, body)
            .await
    }

    async fn get(&self, region: &str, name: &str) -> (u16, Value) {
        self.control_plane(
            "/identities/GetOauth2CredentialProvider",
            region,
            &json!({"name": name}),
        )
        .await
    }

    /// Put a secret of the CALLER's own in `region` holding the handed-over value under
    /// `json_key`, so an external row references a secret Secrets Manager really holds.
    async fn create_caller_secret(&self, case: &str, region: &str, name: &str, json_key: &str) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/secretsmanager/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{region}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", "secretsmanager.CreateSecret")
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(&json!({
                    "Name": name,
                    "SecretString": json!({json_key: HANDED_OVER_SECRET}).to_string(),
                    // A create stakes the secret's initial version, which a raw HTTP
                    // request has to name.
                    "ClientRequestToken": client_request_token(name),
                }))
                .expect("the request body is serializable"),
            ))
            .expect("a valid request");
        let (status, created) = answer(self.router.dispatch(request).await);
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateSecret for {name:?}, got {status} \
             ({created})"
        );
    }
}

/// A UUID-shaped `ClientRequestToken` derived from the secret's name, so each create of this
/// suite stakes its version on a distinct token of the form Secrets Manager asks for.
fn client_request_token(name: &str) -> String {
    let digits: String = name
        .bytes()
        .map(|byte| format!("{:02x}", byte % 16))
        .collect::<String>()
        .chars()
        .cycle()
        .take(32)
        .collect();
    format!(
        "{}-{}-{}-{}-{}",
        &digits[0..8],
        &digits[8..12],
        &digits[12..16],
        &digits[16..20],
        &digits[20..32]
    )
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

/// One provider a row of the first table creates: the name it is minted under, the Region
/// the request is signed for, the vendor and the configuration that vendor selects, and
/// whether the client secret is the caller's own.
struct Row {
    case: &'static str,
    name: String,
    region: &'static str,
    vendor: &'static str,
    /// The `Oauth2ProviderConfigInput` member the vendor selects, and the members of it
    /// other than the client secret.
    config_member: &'static str,
    config: Value,
    /// `Some(jsonKey)` for a provider referencing the caller's own secret under that key.
    external_json_key: Option<&'static str>,
}

impl Row {
    /// The name the caller's own secret is held under, for an external row.
    fn caller_secret_name(&self) -> String {
        format!("caller/{}/clientSecret", self.name)
    }

    /// This row's create request, with any referenced secret already in Secrets Manager.
    async fn create_request(&self, emulator: &Emulator) -> Value {
        let mut config = self.config.clone();
        match self.external_json_key {
            Some(json_key) => {
                let secret_name = self.caller_secret_name();
                emulator
                    .create_caller_secret(self.case, self.region, &secret_name, json_key)
                    .await;
                config["clientSecretConfig"] =
                    json!({"secretId": secret_name, "jsonKey": json_key});
                config["clientSecretSource"] = json!(SOURCE_EXTERNAL);
            }
            None => config["clientSecret"] = json!(HANDED_OVER_SECRET),
        }
        json!({
            "name": self.name,
            "credentialProviderVendor": self.vendor,
            "oauth2ProviderConfigInput": {self.config_member.to_string(): config},
        })
    }

    /// This row's update request: ANOTHER vendor with the member that vendor selects and
    /// another client, so the update replaces the whole configuration of the provider.
    fn update_request(&self) -> Value {
        json!({
            "name": self.name,
            "credentialProviderVendor": "SlackOauth2",
            "oauth2ProviderConfigInput": {"slackOauth2ProviderConfig": {
                "clientId": "the-rotated-client",
                "clientSecret": HANDED_OVER_SECRET,
            }},
        })
    }
}

/// The rows the provider table drives, spanning the vendor and the configuration member it
/// selects, who manages the client secret, the name grammar
/// (`smithy_query(query="shape", shape="CredentialProviderName")` — length 1..128,
/// `^[a-zA-Z0-9\-_]+$`) and the Region signed for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a built-in vendor under the shortest name the model admits",
            name: "g".to_string(),
            region: "us-west-2",
            vendor: "GoogleOauth2",
            config_member: "googleOauth2ProviderConfig",
            config: json!({"clientId": "a-google-client"}),
            external_json_key: None,
        },
        Row {
            case: "another built-in vendor, signed for another Region",
            name: "salesforce-provider".to_string(),
            region: "eu-central-1",
            vendor: "SalesforceOauth2",
            config_member: "salesforceOauth2ProviderConfig",
            config: json!({"clientId": "a-salesforce-client"}),
            external_json_key: None,
        },
        Row {
            case: "the custom member, naming its own authorization server",
            name: "custom_provider-1".to_string(),
            region: "us-east-1",
            vendor: "CustomOauth2",
            config_member: "customOauth2ProviderConfig",
            config: json!({
                "clientId": "a-custom-client",
                "oauthDiscovery": {
                    "discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}"),
                },
            }),
            external_json_key: None,
        },
        Row {
            case: "the included member with the caller's own secret, under the longest name \
                   the model admits",
            // 128 characters: the maximum `CredentialProviderName` admits.
            name: format!("included-provider-{}", "x".repeat(110)),
            region: "ap-southeast-2",
            vendor: "OktaOauth2",
            config_member: "includedOauth2ProviderConfig",
            config: json!({
                "clientId": "an-okta-client",
                "issuer": "https://dev-1.okta.example.com",
            }),
            external_json_key: Some("theClientSecret"),
        },
    ]
}

/// Every provider of the account publishes the served endpoint's one OAuth2 callback
/// address: the create reports it, every read reports the same one, and an update that
/// replaces the whole configuration leaves it as it was.
#[tokio::test]
async fn every_response_declaring_a_callback_url_publishes_the_served_endpoint_s_callback() {
    let emulator = Emulator::new();
    let expected = expected_callback_url(&emulator.served_base);

    for row in rows() {
        let case = row.case;
        let request = row.create_request(&emulator).await;
        let (status, created) = emulator.create(row.region, &request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from the create, got {status} ({created})"
        );
        assert_eq!(
            created["callbackUrl"],
            json!(expected),
            "case {case:?}: expected the create to publish {expected:?}, got {}",
            created["callbackUrl"]
        );

        // The read reports the address the create published, twice over: a read never
        // re-mints it.
        for attempt in 1..=2 {
            let (status, fetched) = emulator.get(row.region, &row.name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from read {attempt}, got {status} ({fetched})"
            );
            assert_eq!(
                fetched["callbackUrl"],
                json!(expected),
                "case {case:?}: expected read {attempt} to report {expected:?}, got {}",
                fetched["callbackUrl"]
            );
        }

        // The update replaces the whole configuration — another vendor, another client —
        // and publishes the same callback, which the read after it still reports.
        let (status, updated) = emulator.update(row.region, &row.update_request()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the update, got {status} ({updated})"
        );
        assert_eq!(
            updated["callbackUrl"],
            json!(expected),
            "case {case:?}: expected the update to publish {expected:?}, got {}",
            updated["callbackUrl"]
        );
        let (status, after) = emulator.get(row.region, &row.name).await;
        assert_eq!(
            (status, &after["callbackUrl"]),
            (200, &json!(expected)),
            "case {case:?}: expected the read after the update to report {expected:?}, got \
             {after}"
        );
        assert_ne!(
            after["oauth2ProviderConfigOutput"], created["oauth2ProviderConfigOutput"],
            "case {case:?}: the update was supposed to replace the configuration, got {after}"
        );
    }
}

/// The published callback names the endpoint this emulator is SERVED at: the base reported
/// as the endpoint, with this plane's callback path appended once, for every form that base
/// can take.
#[tokio::test]
async fn the_published_callback_url_names_the_endpoint_the_emulator_is_served_at() {
    // (case, the base the emulator is served at, or `None` for an emulator on no port)
    let bases: [(&str, Option<&str>); 5] = [
        ("an emulator served on no port at all", None),
        (
            "a loopback authority with a port",
            Some("https://127.0.0.1:9297"),
        ),
        (
            "a named host with a port",
            Some("https://emulator.example:8443"),
        ),
        ("an IPv6 literal", Some("https://[::1]:9443")),
        (
            "a base written with a trailing separator",
            Some("https://emulator.example:8443/"),
        ),
    ];

    for (case, base) in bases {
        let emulator = Emulator::served_at(base);
        let expected = expected_callback_url(&emulator.served_base);
        let region = "us-west-2";
        let request = json!({
            "name": "endpoint-provider",
            "credentialProviderVendor": "GithubOauth2",
            "oauth2ProviderConfigInput": {"githubOauth2ProviderConfig": {
                "clientId": "a-github-client",
                "clientSecret": HANDED_OVER_SECRET,
            }},
        });
        let (status, created) = emulator.create(region, &request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from the create, got {status} ({created})"
        );
        assert_eq!(
            created["callbackUrl"],
            json!(expected),
            "case {case:?}: expected the create to publish {expected:?}, got {}",
            created["callbackUrl"]
        );
        // The path is appended ONCE, and to the base as the endpoint reports it: no empty
        // segment, and the published value is an absolute `https` URL.
        let published = created["callbackUrl"].as_str().unwrap_or_default();
        assert!(
            published.starts_with("https://")
                && published.ends_with(CALLBACK_PATH)
                && !published.trim_start_matches("https://").contains("//"),
            "case {case:?}: expected one well-formed address ending in {CALLBACK_PATH:?}, got \
             {published:?}"
        );

        let (status, fetched) = emulator.get(region, "endpoint-provider").await;
        assert_eq!(
            (status, &fetched["callbackUrl"]),
            (200, &json!(expected)),
            "case {case:?}: expected the read to report {expected:?}, got {fetched}"
        );
    }
}
