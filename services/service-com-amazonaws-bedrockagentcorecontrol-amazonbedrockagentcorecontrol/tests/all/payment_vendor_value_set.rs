// SPDX-License-Identifier: CC0-1.0

//! The value set this control plane ENUMERATES when it refuses a payment credential
//! provider's `credentialProviderVendor`, end to end over HTTP.
//!
//! The rule, one level above the one recorded request: a constraint clause about an
//! enumerated member enumerates the value set the SERVICE publishes for that member's type,
//! which for a type the shipped model's snapshot lags is wider than the set that snapshot
//! declares — and it is the same enumeration wherever a value outside the type arrives,
//! because the set belongs to the type and not to the operation that carried the value. The
//! published set of `PaymentCredentialProviderVendorType` is
//! `crate::state::PUBLISHED_PAYMENT_VENDORS` (`[Stripe, CoinbaseCDP, StripePrivy]`), while
//! the shipped model declares `[CoinbaseCDP, StripePrivy]`
//! (`smithy_query(query="shape",shape="PaymentCredentialProviderVendorType")`).
//!
//! Only the SENTENCE is the published set's: what ADMITS a value is still the model, so the
//! rows also pin the two consequences of that — a value the published set names and the
//! model does not (`Stripe`) is enumerated and still refused, and every value the model
//! does declare is still accepted.
//!
//! The rows differ along what the rule generalises over: the operation carrying the member
//! (both of this model's members that target the type — `smithy_query(query="shape",
//! shape="CreatePaymentCredentialProviderRequest,UpdatePaymentCredentialProviderRequest")`),
//! the submitted value (outside both sets, inside the published set alone, a wrong case of
//! an accepted value, empty, surrounded by whitespace, and far longer than any value), and
//! the vendor configuration the request carried beside it. A second enumerated type that
//! the shipped model declares with the SAME values (`PaymentConnectorType`) is asserted to
//! keep the model's own set, so the rule is pinned to the type and cannot be passed by
//! restating every enum of this endpoint.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// The clause every row of the vendor table expects: the member, and the published set in
/// the order this plane lists it. The `/identities` family states no value of the caller's,
/// which is the echo policy `src/service/validation.rs` states for it.
const PUBLISHED_VENDOR_CLAUSE: &str = "1 validation error detected: Value at \
                                       'credentialProviderVendor' failed to satisfy \
                                       constraint: Member must satisfy enum value set: \
                                       [Stripe, CoinbaseCDP, StripePrivy]";

/// The sentence this plane refuses a provider no name of this account resolves to with,
/// which is how a row checks that a refused create left nothing behind.
const NOT_FOUND: &str = "PaymentCredentialProvider not found";

/// A credential a row hands the service, which never reaches a response. The Coinbase CDP
/// `apiKeySecret` is in the key format that vendor issues one in — the base64 of an Ed25519
/// private key (the 32-byte seed followed by the public key it derives) — because the
/// create refuses a `MANAGED` value that is not such a key
/// (`tests/all/identity_policy_and_rate_limit_refusals.rs` owns that
/// rule); every other credential here is opaque to the service.
const API_KEY_SECRET: &str =
    "YWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjZA==";
const WALLET_SECRET: &str = "Y2RwLXdhbGxldC1zZWNyZXQ=";
const APP_SECRET: &str = "privy-app-secret-value";
const PRIVATE_KEY: &str = "privy-authorization-private-key-value";

/// One answer from the emulator: its status, its `x-amzn-errortype` and its parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            error_type: response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            body: if response.body().is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(response.body()).unwrap_or(Value::Null)
            },
        }
    }

    /// The `message` the refusal states.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator the rows run against: the control plane and Secrets Manager, on one engine,
/// because a provider's credentials live in that service.
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

    /// One `restJson1` request to the control plane. The credential scope names
    /// `bedrock-agentcore`, which is what the router routes on.
    async fn control(&self, uri: &str, body: &Value) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
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
        Answer::of(self.router.dispatch(request).await)
    }

    async fn get_provider(&self, name: &str) -> Answer {
        self.control(
            "/identities/GetPaymentCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }
}

/// The two `PaymentProviderConfigurationInput` members a request may carry
/// (`smithy_query(query="shape",shape="PaymentProviderConfigurationInput")`), so a row can
/// vary the configuration beside the vendor it submits.
fn coinbase_configuration() -> Value {
    json!({"coinbaseCdpConfiguration": {
        "apiKeyId": "cdp_api_key_1",
        "apiKeySecret": API_KEY_SECRET,
        "walletSecret": WALLET_SECRET,
    }})
}

fn privy_configuration() -> Value {
    json!({"stripePrivyConfiguration": {
        "appId": "privy_app_1",
        "appSecret": APP_SECRET,
        "authorizationPrivateKey": PRIVATE_KEY,
        "authorizationId": "privy_authorization_1",
    }})
}

/// One row of the vendor table: where the member was carried, what was in it and what was
/// beside it.
struct VendorRow {
    case: &'static str,
    /// The modelled route the row posts to.
    uri: &'static str,
    /// What the row put in `credentialProviderVendor`.
    vendor: String,
    /// The configuration member carried beside it.
    configuration: Value,
}

/// Every member of this model that targets `PaymentCredentialProviderVendorType`, over the
/// values the rule generalises over.
fn vendor_rows() -> Vec<VendorRow> {
    let create = "/identities/CreatePaymentCredentialProvider";
    let update = "/identities/UpdatePaymentCredentialProvider";
    vec![
        VendorRow {
            case: "a create of a vendor outside both sets",
            uri: create,
            vendor: "NotARealPaymentVendor".to_string(),
            configuration: coinbase_configuration(),
        },
        VendorRow {
            case: "a create of the value the published set names and the model does not",
            uri: create,
            vendor: "Stripe".to_string(),
            configuration: coinbase_configuration(),
        },
        VendorRow {
            case: "a create of a wrong case of an accepted value",
            uri: create,
            vendor: "coinbasecdp".to_string(),
            configuration: coinbase_configuration(),
        },
        VendorRow {
            case: "a create of an empty vendor",
            uri: create,
            vendor: String::new(),
            configuration: privy_configuration(),
        },
        VendorRow {
            case: "a create of an accepted value surrounded by whitespace",
            uri: create,
            vendor: " StripePrivy ".to_string(),
            configuration: privy_configuration(),
        },
        VendorRow {
            case: "a create of a value far longer than any vendor",
            uri: create,
            vendor: "v".repeat(512),
            configuration: coinbase_configuration(),
        },
        VendorRow {
            case: "an update of a vendor outside both sets",
            uri: update,
            vendor: "NotARealPaymentVendor".to_string(),
            configuration: privy_configuration(),
        },
        VendorRow {
            case: "an update of the value the published set names and the model does not",
            uri: update,
            vendor: "Stripe".to_string(),
            configuration: privy_configuration(),
        },
        VendorRow {
            case: "an update of a wrong case of an accepted value",
            uri: update,
            vendor: "STRIPEPRIVY".to_string(),
            configuration: coinbase_configuration(),
        },
    ]
}

/// Every member targeting the vendor type enumerates the published set, whatever was
/// submitted and whichever operation carried it.
#[tokio::test]
async fn a_refused_vendor_enumerates_the_published_value_set() {
    let emulator = Emulator::new();
    for row in vendor_rows() {
        let name = "probe-provider";
        let refused = emulator
            .control(
                row.uri,
                &json!({
                    "name": name,
                    "credentialProviderVendor": row.vendor,
                    "providerConfigurationInput": row.configuration,
                }),
            )
            .await;
        let case = row.case;
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} {:?} ({})",
            refused.status,
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.message(),
            PUBLISHED_VENDOR_CLAUSE,
            "case {case:?}: expected {PUBLISHED_VENDOR_CLAUSE:?}, got {:?}",
            refused.message()
        );

        // A refused request establishes nothing: the name it addressed still carries no
        // provider, so the enumeration is the whole of what the caller got.
        let read = emulator.get_provider(name).await;
        assert_eq!(
            (read.status, read.message()),
            (404, NOT_FOUND.to_string()),
            "case {case:?}: a refused request leaves no provider behind, got {} ({})",
            read.status,
            read.body
        );
    }
}

/// Every value the shipped model declares is still ACCEPTED: the published set is what a
/// refusal names, never what the service admits.
#[tokio::test]
async fn every_vendor_the_model_declares_is_still_accepted() {
    // The model's own value set, with the configuration member each vendor's credentials
    // are carried in (`smithy_query(query="shape",
    // shape="PaymentCredentialProviderVendorType,PaymentProviderConfigurationInput")`).
    let rows = [
        ("CoinbaseCDP", coinbase_configuration()),
        ("StripePrivy", privy_configuration()),
    ];
    for (vendor, configuration) in rows {
        let emulator = Emulator::new();
        let name = format!("accepted-{}", vendor.to_ascii_lowercase());
        let created = emulator
            .control(
                "/identities/CreatePaymentCredentialProvider",
                &json!({
                    "name": name,
                    "credentialProviderVendor": vendor,
                    "providerConfigurationInput": configuration,
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {vendor:?}: a declared vendor is accepted, got {} ({})",
            created.status, created.body
        );
        let read = emulator.get_provider(&name).await;
        assert_eq!(
            (
                read.status,
                read.body
                    .get("credentialProviderVendor")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            ),
            (200, vendor),
            "case {vendor:?}: the provider reads back under the vendor it was created with, \
             got {} ({})",
            read.status,
            read.body
        );
    }
}

/// Another enumerated type the shipped model declares with the SAME values keeps the
/// model's own set: the published set belongs to one type, not to every enum of this
/// endpoint.
#[tokio::test]
async fn another_type_declared_with_the_same_values_keeps_the_models_set() {
    let emulator = Emulator::new();
    // `PaymentConnectorType`'s own values (`smithy_query(query="shape",
    // shape="PaymentConnectorType")` -> `[CoinbaseCDP, StripePrivy]`), carried in
    // `CreatePaymentConnector`'s `type`. The `/payments` family quotes the submitted value,
    // which is that family's echo policy rather than anything this rule decides.
    let rows = [
        ("a connector type outside the set", "NotAConnectorType"),
        (
            "the value the vendor type publishes and this one does not",
            "Stripe",
        ),
    ];
    for (case, connector_type) in rows {
        let refused = emulator
            .control(
                // A `paymentManagerId` inside its modelled pattern and 12..211 length, so
                // the refusal states the one clause the row is about.
                "/payments/managers/manager-0123456789/connectors",
                &json!({
                    "name": "probeConnector",
                    "type": connector_type,
                    "credentialProviderConfigurations": [],
                }),
            )
            .await;
        let expected = format!(
            "1 validation error detected: Value '{connector_type}' at 'type' failed to \
             satisfy constraint: Member must satisfy enum value set: [CoinbaseCDP, \
             StripePrivy]"
        );
        assert_eq!(
            (refused.status, refused.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} {:?} ({})",
            refused.status,
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            refused.message()
        );
    }
}
