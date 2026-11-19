// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what `CreateOauth2CredentialProvider` MINTS.
//!
//! One rule is under test: a create mints exactly one OAuth2 credential provider per
//! `name` in the account's token vault — it answers HTTP 201 with the vault-scoped ARN
//! ending in that name in the REQUEST's region, with the client secret named as a secret
//! Secrets Manager really holds rather than echoed, and with a `Status` of `READY`
//! (`smithy_query(query="shape", shape="Status")` lists `CREATING | CREATE_FAILED |
//! UPDATING | UPDATE_FAILED | READY | DELETING | DELETE_FAILED`; nothing is left for the
//! clock to owe a provider whose secret the create already resolved) — and the name
//! IDENTIFIES it, so the provider reads back under that name and a later create under a
//! name the account already holds is refused `400 ValidationException`, leaving the held
//! provider exactly as it was.
//!
//! The rows span what the rule generalises over: the `CredentialProviderName` grammar
//! (`smithy_query(query="shape", shape="CredentialProviderName")` — length 1..128,
//! `^[a-zA-Z0-9\-_]+$`) at its minimum, with both separators, and at its maximum; the
//! vendor and the `Oauth2ProviderConfigInput` member it selects (a built-in vendor, the
//! custom member, the included member); whether the client secret is the service's to
//! manage or the caller's own in Secrets Manager; the tags the request carries; and the
//! region it is signed for.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, because a provider's client secret is stored
//! in a real secret there.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The token vault every credential provider of this account belongs to, as the ARN
/// `CredentialProviderArnType` describes nests it:
/// `…:token-vault/{tokenVaultId}/oauth2credentialprovider/{name}`.
const VAULT: &str = "default";

/// The ARN namespace this service's resources are addressed in.
const ARN_NAMESPACE: &str = "bedrock-agentcore";

/// The two `SecretSourceType` values (`smithy_query(query="shape",
/// shape="SecretSourceType")`).
const SOURCE_MANAGED: &str = "MANAGED";
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The `Status` a provider whose secret the create already resolved carries.
const STATUS_READY: &str = "READY";

/// The published location of an authorization server's metadata document, which is one of
/// the two endings `Oauth2Discovery$discoveryUrl`'s modelled pattern admits.
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";

/// The client secret value a managed row hands over, which no response may carry.
const HANDED_OVER_SECRET: &str = "the-client-secret-value";

/// This service and Secrets Manager over one shared engine, behind the same
/// `MultiServiceRouter` the production router builds.
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

    async fn get(&self, region: &str, name: &str) -> (u16, Value) {
        self.control_plane(
            "/identities/GetOauth2CredentialProvider",
            region,
            &json!({"name": name}),
        )
        .await
    }

    async fn list(&self, region: &str) -> (u16, Value) {
        self.control_plane(
            "/identities/ListOauth2CredentialProviders",
            region,
            &json!({}),
        )
        .await
    }

    /// Put a secret of the CALLER's own in `region` holding `value` under `json_key`, so
    /// an external row references a secret Secrets Manager really holds.
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
                    // request has to name: `ClientRequestToken` is 32..64 characters
                    // (`smithy_query(query="operation", operation="CreateSecret",
                    // model_path="services/service-com-amazonaws-secretsmanager-\
                    // secretsmanager/smithy-model/\
                    // com-amazonaws-secretsmanager-secretsmanager.json")`).
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

/// A UUID-shaped `ClientRequestToken` derived from the secret's name, so each create of
/// this suite stakes its version on a distinct token of the form Secrets Manager asks for.
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

/// The ARN a provider named `name` carries in `region`: the account the emulator answers
/// for, the vault it belongs to and the name the caller chose.
fn expected_provider_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:{ARN_NAMESPACE}:{region}:{}:token-vault/{VAULT}/oauth2credentialprovider/{name}",
        engine::emulator_account_id()
    )
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

/// One provider a row creates: the name it is minted under, the region the request is
/// signed for, the vendor and the configuration that vendor selects, whether the client
/// secret is the caller's own, and the tags the request carries.
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
    tags: Option<Value>,
}

impl Row {
    /// The effective `clientSecretSource`: `EXTERNAL` where the request references the
    /// caller's own secret, `MANAGED` where it hands the value over.
    fn expected_source(&self) -> &'static str {
        match self.external_json_key {
            Some(_) => SOURCE_EXTERNAL,
            None => SOURCE_MANAGED,
        }
    }

    /// The name the caller's own secret is held under, for an external row.
    fn caller_secret_name(&self) -> String {
        format!("caller/{}/clientSecret", self.name)
    }

    /// This row's create request, with the referenced secret already in Secrets Manager.
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
        let mut body = json!({
            "name": self.name,
            "credentialProviderVendor": self.vendor,
            "oauth2ProviderConfigInput": {self.config_member.to_string(): config},
        });
        if let Some(tags) = &self.tags {
            body["tags"] = tags.clone();
        }
        body
    }
}

/// The rows the served table drives, spanning the name grammar, the vendor and the
/// configuration member it selects, who manages the client secret, the tags carried and
/// the region signed for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a built-in vendor under the shortest name the model admits",
            // `CredentialProviderName` is 1..128 characters.
            name: "g".to_string(),
            region: "us-east-1",
            vendor: "GoogleOauth2",
            config_member: "googleOauth2ProviderConfig",
            config: json!({"clientId": "a-google-client"}),
            external_json_key: None,
            tags: None,
        },
        Row {
            case: "a custom provider whose name carries both separators the pattern admits",
            name: "custom-provider_1".to_string(),
            region: "eu-west-1",
            vendor: "CustomOauth2",
            config_member: "customOauth2ProviderConfig",
            config: json!({
                "clientId": "a-custom-client",
                "oauthDiscovery": {
                    "discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}"),
                },
            }),
            external_json_key: None,
            tags: Some(json!({"environment": "test"})),
        },
        Row {
            case: "an included provider under the longest name the model admits",
            // 128 characters: the maximum `CredentialProviderName` admits.
            name: format!("included-provider-{}", "x".repeat(110)),
            region: "ap-southeast-2",
            vendor: "OktaOauth2",
            config_member: "includedOauth2ProviderConfig",
            config: json!({
                "clientId": "an-okta-client",
                "issuer": "https://dev-1.okta.example.com",
            }),
            external_json_key: None,
            tags: Some(json!({"environment": "test", "owner": "the-team"})),
        },
        Row {
            case: "a built-in vendor referencing the caller's own client secret",
            name: "github-external".to_string(),
            region: "us-west-2",
            vendor: "GithubOauth2",
            config_member: "githubOauth2ProviderConfig",
            config: json!({"clientId": "a-github-client"}),
            external_json_key: Some("theClientSecret"),
            tags: None,
        },
    ]
}

/// A create mints the provider the request asked for: the vault-scoped ARN in the
/// request's region, the secret named rather than echoed, `READY`, and the provider reads
/// back under its name reporting exactly what the create answered.
#[tokio::test]
async fn a_create_mints_a_provider_under_its_name_and_the_read_reports_it() {
    let emulator = Emulator::new();

    for row in rows() {
        let case = row.case;
        let request = row.create_request(&emulator).await;
        let (status, created) = emulator.create(row.region, &request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from the create, got {status} ({created})"
        );
        assert_eq!(
            created["name"], json!(row.name),
            "case {case:?}: the create reports another name, got {created}"
        );
        assert_eq!(
            created["credentialProviderArn"],
            json!(expected_provider_arn(row.region, &row.name)),
            "case {case:?}: unexpected provider ARN, got {created}"
        );
        assert_eq!(
            created["status"], json!(STATUS_READY),
            "case {case:?}: expected status {STATUS_READY}, got {created}"
        );
        assert_eq!(
            created["clientSecretSource"], json!(row.expected_source()),
            "case {case:?}: expected clientSecretSource {}, got {created}",
            row.expected_source()
        );
        // The JSON key is reported only where the caller's own `SecretReference` named
        // one; the layout of a secret the SERVICE manages is not the caller's to read.
        assert_eq!(
            created["clientSecretJsonKey"],
            row.external_json_key.map_or(Value::Null, |key| json!(key)),
            "case {case:?}: unexpected clientSecretJsonKey, got {created}"
        );
        // The provider names a secret of the emulator's own Secrets Manager, and the
        // value the caller handed over is in that secret rather than in the answer.
        let secret_arn = created["clientSecretArn"]["secretArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            secret_arn.starts_with(&format!("arn:aws:secretsmanager:{}:", row.region)),
            "case {case:?}: the create names no Secrets Manager secret of this region, got \
             {secret_arn:?} in {created}"
        );
        assert!(
            !created.to_string().contains(HANDED_OVER_SECRET),
            "case {case:?}: the create echoed the client secret, in {created}"
        );

        // The provider is really held: the read answers under the same name with the
        // members the create reported, the vendor the create declared, and the two
        // instants a freshly created provider carries.
        let (status, fetched) = emulator.get(row.region, &row.name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read, got {status} ({fetched})"
        );
        for member in [
            "name",
            "credentialProviderArn",
            "status",
            "clientSecretArn",
            "clientSecretSource",
            "clientSecretJsonKey",
            "oauth2ProviderConfigOutput",
        ] {
            assert_eq!(
                fetched[member], created[member],
                "case {case:?}: the read and the create disagree on {member:?}, got {fetched} \
                 after {created}"
            );
        }
        assert_eq!(
            fetched["credentialProviderVendor"], json!(row.vendor),
            "case {case:?}: the read reports another vendor, got {fetched}"
        );
        assert_eq!(
            fetched["createdTime"], fetched["lastUpdatedTime"],
            "case {case:?}: a provider nothing has updated reports two instants, got {fetched}"
        );
        assert!(
            fetched["oauth2ProviderConfigOutput"][row.config_member]["clientId"]
                == row.config["clientId"],
            "case {case:?}: the read reports another client, got {fetched}"
        );
    }

    // Every provider created above is in the account's listing, once each.
    let (status, listed) = emulator.list("us-east-1").await;
    let mut names = listed_names(&listed);
    names.sort();
    let mut expected: Vec<String> = rows().into_iter().map(|row| row.name).collect();
    expected.sort();
    assert_eq!(
        (status, names),
        (200, expected),
        "the listing reports every created provider once, got {listed}"
    );
}

/// A create whose `name` the account already holds is refused `400 ValidationException`,
/// and the held provider is left exactly as the first create minted it.
///
/// The rows differ along how the second request differs from the first — the identical
/// request, another client of the same vendor, another vendor with the member that vendor
/// selects, and the caller's own secret instead of a managed one — so the rule is about
/// the NAME being taken, not about the rest of the request.
#[tokio::test]
async fn a_create_under_a_held_name_is_refused_and_leaves_the_held_provider_untouched() {
    let emulator = Emulator::new();
    let held = "held-provider";
    let region = "us-east-1";
    let first = json!({
        "name": held,
        "credentialProviderVendor": "GoogleOauth2",
        "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": {
            "clientId": "the-first-client",
            "clientSecret": HANDED_OVER_SECRET,
        }},
    });
    let (status, created) = emulator.create(region, &first).await;
    assert_eq!(
        status, 201,
        "the first create is served, got {status} ({created})"
    );
    let (status, before) = emulator.get(region, held).await;
    assert_eq!(
        status, 200,
        "the held provider reads back, got {status} ({before})"
    );

    // (case, the second request under the same name)
    let rows: [(&str, Value); 4] = [
        ("the identical request again", first.clone()),
        (
            "another client of the same vendor",
            json!({
                "name": held,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": {
                    "clientId": "the-second-client",
                    "clientSecret": HANDED_OVER_SECRET,
                }},
            }),
        ),
        (
            "another vendor, with the member that vendor selects",
            json!({
                "name": held,
                "credentialProviderVendor": "SlackOauth2",
                "oauth2ProviderConfigInput": {"slackOauth2ProviderConfig": {
                    "clientId": "a-slack-client",
                    "clientSecret": HANDED_OVER_SECRET,
                }},
            }),
        ),
        (
            "the custom member with the caller's own secret",
            json!({
                "name": held,
                "credentialProviderVendor": "CustomOauth2",
                "oauth2ProviderConfigInput": {"customOauth2ProviderConfig": {
                    "clientId": "a-custom-client",
                    "oauthDiscovery": {
                        "discoveryUrl": format!("https://auth.example.com{OPENID_CONFIGURATION}"),
                    },
                    "clientSecretSource": SOURCE_EXTERNAL,
                    "clientSecretConfig": {
                        "secretId": "caller/held/clientSecret",
                        "jsonKey": "theClientSecret",
                    },
                }},
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, refused) = emulator.create(region, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} ({refused})"
        );
        assert_eq!(
            refused["message"],
            json!(format!("Credential provider with name: {held} already exists")),
            "case {case:?}: the refusal does not name the provider the account holds, got \
             {refused}"
        );

        let (status, after) = emulator.get(region, held).await;
        assert_eq!(
            (status, &after),
            (200, &before),
            "case {case:?}: the refused create changed the held provider, got {status} ({after})"
        );
        assert_eq!(
            listed_names(&emulator.list(region).await.1),
            vec![held.to_string()],
            "case {case:?}: the account holds other than one provider of that name"
        );
    }
}
