// SPDX-License-Identifier: CC0-1.0

//! WHICH payment vendor secrets the key-format rule reaches, end to end over HTTP.
//!
//! The rule the sibling suite `identity_policy_and_rate_limit_refusals.rs` pins is that a
//! `MANAGED` Coinbase CDP `apiKeySecret` must be a base64-encoded Ed25519 private key. This
//! suite pins its SCOPE, which is the other half of the same rule: the check reaches exactly
//! the secrets whose vendor fixes what kind of key they are, and exactly the values the
//! REQUEST hands over.
//!
//! * the three other vendor secrets (`walletSecret`, `appSecret`,
//!   `authorizationPrivateKey`) are opaque to this service — the vendor states no key kind
//!   for them, so they are bounded by their own `@pattern` and `@length` alone
//!   (`smithy_query(query="shape",shape="CoinbaseCdpConfigurationInput,\
//!   StripePrivyConfigurationInput")`) — and a create carrying a value in them that is not
//!   a key at all is ACCEPTED and reads back;
//! * an `EXTERNAL` `apiKeySecret` hands over no key: the request carries a reference to the
//!   caller's own secret, whose value is theirs, so the create is accepted whatever that
//!   secret holds;
//! * the same not-a-key value in the one member whose vendor DOES fix a format is refused.
//!
//! The rows differ along the vendor, the member the not-a-key value sits in, and who
//! manages the secret.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A Coinbase CDP `apiKeySecret` in the key format that vendor issues one in: the base64 of
/// an Ed25519 private key (the 32-byte seed followed by the public key it derives), for the
/// rows whose point is elsewhere.
const CDP_API_KEY_SECRET: &str =
    "YWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjZA==";

/// A value that is not a key in any vendor's format: inside every secret member's own
/// `@pattern` (`^[a-zA-Z0-9+/=\-_\s]*$`) and base64 of nothing — nine characters cannot be
/// a base64 quantum.
const NOT_A_KEY: &str = "not-a-key";

/// The sentence the one member whose vendor fixes a key format is refused with.
const INVALID_API_KEY_SECRET: &str =
    "Invalid apiKeySecret format: Expected base64-encoded Ed25519 private key";

/// One answer from the emulator: its status and its parsed body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: if response.body().is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(response.body()).unwrap_or(Value::Null)
            },
        }
    }

    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator the rows run against: this control plane and Secrets Manager, on one engine,
/// because a managed secret is really stored in that service and a referenced one really
/// read from it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// POST one `restJson1` operation of this control plane.
    async fn control(&self, uri: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("host", "bedrock-agentcore-control.amazonaws.com")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
                     aws4_request, SignedHeaders=host, Signature=abc"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// POST one `awsJson1_1` Secrets Manager operation.
    async fn secrets_manager(&self, operation: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{REGION}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("secretsmanager.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/secretsmanager/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    async fn create_provider(&self, body: &Value) -> Answer {
        self.control("/identities/CreatePaymentCredentialProvider", body)
            .await
    }

    async fn get_provider(&self, name: &str) -> Answer {
        self.control(
            "/identities/GetPaymentCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }
}

/// One row: the vendor, the configuration it carries, and the vendor the created provider
/// must report.
struct Row {
    case: &'static str,
    name: &'static str,
    vendor: &'static str,
    configuration: Value,
}

/// A value that is not a key, in a secret whose vendor fixes no key format, is stored: the
/// format rule reaches the secrets the vendor states a kind for and no others.
#[tokio::test]
async fn a_secret_whose_vendor_fixes_no_key_format_takes_any_admitted_value() {
    let rows = vec![
        Row {
            case: "the Coinbase CDP wallet secret, which the vendor fixes no kind for",
            name: "opaque-wallet",
            vendor: "CoinbaseCDP",
            configuration: json!({"coinbaseCdpConfiguration": {
                "apiKeyId": "cdp-api-key",
                "apiKeySecret": CDP_API_KEY_SECRET,
                "walletSecret": NOT_A_KEY,
            }}),
        },
        Row {
            case: "both Stripe Privy secrets, which the vendor fixes no kind for",
            name: "opaque-privy",
            vendor: "StripePrivy",
            configuration: json!({"stripePrivyConfiguration": {
                "appId": "privy-app",
                "authorizationId": "privy-authorization",
                "appSecret": NOT_A_KEY,
                // `DefaultStripePrivyAuthorizationPrivateKeyType`'s pattern admits a
                // `wallet-auth:` prefix (`smithy_query(query="shape",
                // shape="StripePrivyConfigurationInput")`).
                "authorizationPrivateKey": format!("wallet-auth:{NOT_A_KEY}"),
            }}),
        },
    ];

    for row in rows {
        let emulator = Emulator::new();
        let case = row.case;
        let created = emulator
            .create_provider(&json!({
                "name": row.name,
                "credentialProviderVendor": row.vendor,
                "providerConfigurationInput": row.configuration,
            }))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: an opaque secret takes the value as sent, got {} ({})",
            created.status, created.body
        );
        let read = emulator.get_provider(row.name).await;
        assert_eq!(
            (read.status, read.text("credentialProviderVendor")),
            (200, row.vendor.to_string()),
            "case {case:?}: the provider reads back, got {} ({})",
            read.status,
            read.body
        );
    }
}

/// The SAME not-a-key value is refused in the one member whose vendor fixes a key format,
/// and accepted as the caller's own referenced secret — because an `EXTERNAL` secret hands
/// no key over in the request.
#[tokio::test]
async fn the_key_format_rule_reaches_the_value_the_request_hands_over() {
    let emulator = Emulator::new();

    // Handed over in the request: refused, and nothing is created.
    let refused = emulator
        .create_provider(&json!({
            "name": "handed-over-not-a-key",
            "credentialProviderVendor": "CoinbaseCDP",
            "providerConfigurationInput": {"coinbaseCdpConfiguration": {
                "apiKeyId": "cdp-api-key",
                "apiKeySecret": NOT_A_KEY,
                "walletSecret": "bWFuYWdlZC13YWxsZXQ=",
            }},
        }))
        .await;
    assert_eq!(
        (refused.status, refused.text("message")),
        (400, INVALID_API_KEY_SECRET.to_string()),
        "case \"handed over in the request\": expected the modelled 400 for a value that is \
         not the vendor's key, got {} ({})",
        refused.status,
        refused.body
    );
    assert_eq!(
        emulator.get_provider("handed-over-not-a-key").await.status,
        404,
        "case \"handed over in the request\": the refused create left a provider behind"
    );

    // The same value inside the CALLER's own secret: the request hands over a reference,
    // so the create is served and the provider reads back.
    let secret_name = "partner/cdp-api-key";
    let stored = emulator
        .secrets_manager(
            "CreateSecret",
            &json!({
                "Name": secret_name,
                "SecretString": json!({"apiKeySecret": NOT_A_KEY}).to_string(),
                "ClientRequestToken": "11111111-2222-3333-4444-555555555555",
            }),
        )
        .await;
    assert_eq!(
        stored.status, 200,
        "setup: the caller's own secret must be created, got {} ({})",
        stored.status, stored.body
    );

    let created = emulator
        .create_provider(&json!({
            "name": "referenced-not-a-key",
            "credentialProviderVendor": "CoinbaseCDP",
            "providerConfigurationInput": {"coinbaseCdpConfiguration": {
                "apiKeyId": "cdp-api-key",
                "apiKeySecret": "",
                "apiKeySecretSource": "EXTERNAL",
                "apiKeySecretConfig": {"secretId": secret_name, "jsonKey": "apiKeySecret"},
                "walletSecret": "bWFuYWdlZC13YWxsZXQ=",
            }},
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "case \"referenced in the caller's own secret\": the create hands over no key, so it \
         is served, got {} ({})",
        created.status, created.body
    );
    let read = emulator.get_provider("referenced-not-a-key").await;
    assert_eq!(
        (read.status, read.text("credentialProviderVendor")),
        (200, "CoinbaseCDP".to_string()),
        "case \"referenced in the caller's own secret\": the provider reads back, got {} ({})",
        read.status,
        read.body
    );
}
