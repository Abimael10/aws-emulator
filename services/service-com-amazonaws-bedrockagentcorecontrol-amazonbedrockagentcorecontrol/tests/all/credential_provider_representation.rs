// SPDX-License-Identifier: CC0-1.0

//! How this service REPRESENTS a credential provider of the account's token
//! vault: the ARN it is addressed by, and the JSON key it reports for the secret
//! its credential lives in.
//!
//! The rule under test, over every credential-provider plane of this service (the
//! API key provider, the OAuth2 provider and the payment provider):
//!
//! 1. a provider's `credentialProviderArn` is the token-vault path of its own ARN
//!    shape in the ARN namespace of the SERVICE that holds the vault —
//!    `arn:aws:bedrock-agentcore:<region>:<account>:token-vault/<vault>/<kind>/<name>`
//!    — which is the namespace `ConsentPortalIdpConfig.credentialProviderArn`'s
//!    pattern requires of a credential provider's ARN (`smithy_query(query="shape",
//!    shape="ConsentPortalIdpConfig")`) and the one every other resource of this
//!    service is addressed in, rather than the legacy `acps` namespace the
//!    provider-ARN shapes' own patterns still spell;
//! 2. a provider reports a secret's JSON key only where the REQUEST supplied one in
//!    a `SecretReference`: exactly the `jsonKey` the caller named there, and no
//!    member at all for a secret the service manages, whose document layout the
//!    caller never named — every one of these `…JsonKey` members is optional
//!    (`smithy_query(query="shape", shape="CreateApiKeyCredentialProviderResponse")`,
//!    `shape="CoinbaseCdpConfigurationOutput"`);
//! 3. the read reports the same, member for member: the representation belongs to
//!    the stored record, not to the create.
//!
//! The rows span the plane (so the ARN's resource type and the shape the secret is
//! carried in), who manages the secret (the service with the source member left
//! out, the service with it named, the caller by reference), the `jsonKey` the
//! caller named — including one that is exactly the name the service's own managed
//! document uses, so presence cannot be read off the key's spelling — the Region
//! the request is signed for, and, on the payment plane whose provider carries TWO
//! secrets, a provider with one of each.
//!
//! Secrets Manager is mounted beside this service on ONE engine, the shape
//! `core/router` mounts them in, so every referenced secret below is a real secret
//! created through that service's own operations.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The token vault a request naming none addresses, which a provider's ARN nests
/// it under.
const VAULT: &str = "default";

/// The ARN service namespace of the service that holds the token vault
/// (`OPERATIONS.md`'s "ARN namespace: bedrock-agentcore").
const ARN_NAMESPACE: &str = "bedrock-agentcore";

/// `SecretSourceType`'s full value list (`smithy_query(query="shape",
/// shape="SecretSourceType")`).
const SOURCE_MANAGED: &str = "MANAGED";
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// Which credential-provider plane a row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plane {
    ApiKey,
    Oauth2,
    Payment,
}

impl Plane {
    /// The `restJson1` path of this plane's create, which answers 201
    /// (`OPERATIONS.md`).
    fn create_uri(self) -> &'static str {
        match self {
            Self::ApiKey => "/identities/CreateApiKeyCredentialProvider",
            Self::Oauth2 => "/identities/CreateOauth2CredentialProvider",
            Self::Payment => "/identities/CreatePaymentCredentialProvider",
        }
    }

    /// The path of this plane's read, which answers 200.
    fn get_uri(self) -> &'static str {
        match self {
            Self::ApiKey => "/identities/GetApiKeyCredentialProvider",
            Self::Oauth2 => "/identities/GetOauth2CredentialProvider",
            Self::Payment => "/identities/GetPaymentCredentialProvider",
        }
    }

    /// The ARN resource type a provider of this plane is nested under inside the
    /// token vault, from its own ARN shape's pattern
    /// (`smithy_query(query="shape", shape="ApiKeyCredentialProviderArnType")`,
    /// `shape="CredentialProviderArnType"`,
    /// `shape="PaymentCredentialProviderArnType"`).
    fn arn_type(self) -> &'static str {
        match self {
            Self::ApiKey => "apikeycredentialprovider",
            Self::Oauth2 => "oauth2credentialprovider",
            Self::Payment => "paymentcredentialprovider",
        }
    }

    /// Where a response of this plane reports the JSON key of the secret carried
    /// under the `member` member family, as a JSON pointer into the body: at the
    /// top level of the provider's own shape, or, on the payment plane, inside
    /// the vendor configuration the provider's secrets belong to.
    fn json_key_pointer(self, member: &str) -> String {
        match self {
            Self::ApiKey | Self::Oauth2 => format!("/{member}JsonKey"),
            Self::Payment => format!(
                "/providerConfigurationOutput/coinbaseCdpConfiguration/{member}JsonKey"
            ),
        }
    }

    /// Where a response of this plane reports who manages the secret carried
    /// under the `member` member family.
    fn source_pointer(self, member: &str) -> String {
        match self {
            Self::ApiKey | Self::Oauth2 => format!("/{member}Source"),
            Self::Payment => {
                format!("/providerConfigurationOutput/coinbaseCdpConfiguration/{member}Source")
            }
        }
    }
}

/// A Coinbase CDP `apiKeySecret` in the key format that vendor issues one in: the base64
/// of an Ed25519 private key (here the 32-byte seed), which the payment plane's create
/// requires of a secret it is to manage.
const CDP_API_KEY_SECRET: &str = "YWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjYWJjZGU=";

/// ONE secret a row's provider carries.
#[derive(Debug, Clone)]
struct SecretSlot {
    /// The member family the secret's `…Config`, `…Source` and `…JsonKey`
    /// members are named after: `apiKeySecret` on the API key plane,
    /// `clientSecret` on the OAuth2 plane, `apiKeySecret` and `walletSecret` on
    /// the payment plane.
    member: &'static str,
    /// The member a request hands the value over for the SERVICE to manage it
    /// under, which is not always the family's own name (`apiKey` carries the
    /// key of an `apiKeySecret…` family).
    value_member: &'static str,
    /// The value the secret holds. Every character of it is one the payment
    /// plane's secret members admit (`^[a-zA-Z0-9+/=\-_\s]*$`,
    /// `smithy_query(query="shape", shape="CoinbaseCdpConfigurationInput")`), so
    /// one value serves every plane.
    value: &'static str,
    /// The `jsonKey` the request's `SecretReference` names, or `None` when the
    /// request hands the value over for the SERVICE to manage.
    json_key: Option<&'static str>,
    /// Whether the request spells the `…Source` member out rather than leaving
    /// it to be implied.
    source_sent: bool,
}

impl SecretSlot {
    /// The `…Source` this slot ends up with, named or implied.
    fn effective_source(&self) -> &'static str {
        if self.json_key.is_some() {
            SOURCE_EXTERNAL
        } else {
            SOURCE_MANAGED
        }
    }
}

/// One provider a row creates.
struct Row {
    case: &'static str,
    plane: Plane,
    name: &'static str,
    region: &'static str,
    slots: Vec<SecretSlot>,
}

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

    /// Create a secret of the CALLER's own in `region`, holding `value`, and
    /// answer the name it is addressed by.
    async fn create_caller_secret(&self, case: &str, region: &str, name: &str, value: &str) {
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
                    "SecretString": value,
                    // A create stakes the secret's initial version, which a raw
                    // HTTP request has to name: `ClientRequestToken` is 32..64
                    // characters (`smithy_query(query="operation",
                    // operation="CreateSecret",
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

/// A UUID-shaped `ClientRequestToken` derived from the secret's name, so each
/// create of this suite stakes its version on a distinct token of the form
/// Secrets Manager asks for.
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

/// The ARN a provider of `plane` named `name` carries in `region`.
fn provider_arn(plane: Plane, region: &str, name: &str) -> String {
    format!(
        "arn:aws:{ARN_NAMESPACE}:{region}:{ACCOUNT}:token-vault/{VAULT}/{}/{name}",
        plane.arn_type()
    )
}

/// The name the secret a row's external slot references is held under.
fn caller_secret_name(row: &Row, slot: &SecretSlot) -> String {
    format!("caller/{}/{}", row.name, slot.member)
}

/// The create request for `row`, with every referenced secret already in Secrets
/// Manager.
async fn create_request(emulator: &Emulator, row: &Row) -> Value {
    let mut carried = json!({});
    for slot in &row.slots {
        match slot.json_key {
            Some(json_key) => {
                let secret_name = caller_secret_name(row, slot);
                emulator
                    .create_caller_secret(
                        row.case,
                        row.region,
                        &secret_name,
                        &json!({ json_key: slot.value }).to_string(),
                    )
                    .await;
                carried[format!("{}Config", slot.member)] =
                    json!({"secretId": secret_name, "jsonKey": json_key});
            }
            None => carried[slot.value_member] = json!(slot.value),
        }
        if slot.source_sent {
            carried[format!("{}Source", slot.member)] = json!(slot.effective_source());
        }
    }
    let carried = carried.as_object().cloned().unwrap_or_default();

    match row.plane {
        Plane::ApiKey => {
            let mut body = json!({"name": row.name});
            for (member, value) in carried {
                body[member] = value;
            }
            body
        }
        Plane::Oauth2 => {
            let mut config = json!({"clientId": "a-google-oauth2-client"});
            for (member, value) in carried {
                config[member] = value;
            }
            json!({
                "name": row.name,
                "credentialProviderVendor": "GoogleOauth2",
                "oauth2ProviderConfigInput": {"googleOauth2ProviderConfig": config},
            })
        }
        Plane::Payment => {
            let mut config = json!({"apiKeyId": "cdp-api-key-1"});
            for (member, value) in carried {
                config[member] = value;
            }
            json!({
                "name": row.name,
                "credentialProviderVendor": "CoinbaseCDP",
                "providerConfigurationInput": {"coinbaseCdpConfiguration": config},
            })
        }
    }
}

/// Assert `body` represents `row`'s provider as the rule says: its token-vault ARN
/// in this service's own namespace, and one JSON key per secret only where the
/// request named one.
fn assert_representation(case: &str, body: &Value, row: &Row) {
    assert_eq!(
        body["credentialProviderArn"],
        json!(provider_arn(row.plane, row.region, row.name)),
        "case {case:?}: expected the provider's token-vault ARN in this service's own ARN \
         namespace, got body {body}"
    );
    for slot in &row.slots {
        let source = body.pointer(&row.plane.source_pointer(slot.member)).cloned();
        assert_eq!(
            source,
            Some(json!(slot.effective_source())),
            "case {case:?} ({}): expected the effective secret source, got body {body}",
            slot.member
        );
        let reported = body.pointer(&row.plane.json_key_pointer(slot.member)).cloned();
        match slot.json_key {
            Some(json_key) => assert_eq!(
                reported,
                Some(json!(json_key)),
                "case {case:?} ({}): expected the jsonKey the request's secret reference \
                 named, got body {body}",
                slot.member
            ),
            None => assert_eq!(
                reported, None,
                "case {case:?} ({}): a secret the service manages is reported with no JSON \
                 key, got body {body}",
                slot.member
            ),
        }
    }
}

/// The providers the rule is asserted over: one row per thing it generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an API key provider whose secret the service manages, no source sent",
            plane: Plane::ApiKey,
            name: "apikey-managed-implied",
            region: "us-west-2",
            slots: vec![SecretSlot {
                member: "apiKeySecret",
                value_member: "apiKey",
                value: "c2stbGl2ZS1tYW5hZ2Vk",
                json_key: None,
                source_sent: false,
            }],
        },
        Row {
            case: "an API key provider whose secret the service manages, source named",
            plane: Plane::ApiKey,
            name: "apikey_managed_named",
            region: "eu-west-1",
            slots: vec![SecretSlot {
                member: "apiKeySecret",
                value_member: "apiKey",
                value: "c2stbGl2ZS1uYW1lZA==",
                json_key: None,
                source_sent: true,
            }],
        },
        Row {
            case: "an API key provider referencing the caller's secret under the same key \
                   name the service's own managed document uses",
            plane: Plane::ApiKey,
            name: "apikey-external-same-key",
            region: "us-west-2",
            slots: vec![SecretSlot {
                member: "apiKeySecret",
                value_member: "apiKey",
                value: "Y2FsbGVyLW93bmVkLWtleQ==",
                json_key: Some("apiKey"),
                source_sent: true,
            }],
        },
        Row {
            case: "an API key provider referencing the caller's secret under a key of the \
                   caller's own, the source implied by the reference",
            plane: Plane::ApiKey,
            name: "apikey-external-own-key",
            region: "ap-southeast-2",
            slots: vec![SecretSlot {
                member: "apiKeySecret",
                value_member: "apiKey",
                value: "cGFydG5lci1vd25lZC1rZXk=",
                json_key: Some("partner_token"),
                source_sent: false,
            }],
        },
        Row {
            case: "an OAuth2 provider whose client secret the service manages",
            plane: Plane::Oauth2,
            name: "oauth2-managed",
            region: "us-west-2",
            slots: vec![SecretSlot {
                member: "clientSecret",
                value_member: "clientSecret",
                value: "Z29vZ2xlLW1hbmFnZWQtc2VjcmV0",
                json_key: None,
                source_sent: false,
            }],
        },
        Row {
            case: "an OAuth2 provider referencing the caller's own client secret",
            plane: Plane::Oauth2,
            name: "oauth2-external",
            region: "eu-west-1",
            slots: vec![SecretSlot {
                member: "clientSecret",
                value_member: "clientSecret",
                value: "Y2FsbGVyLWNsaWVudC1zZWNyZXQ=",
                json_key: Some("vault_entry"),
                source_sent: true,
            }],
        },
        Row {
            case: "a payment provider whose two secrets the service both manages",
            plane: Plane::Payment,
            name: "payment-both-managed",
            region: "us-west-2",
            slots: vec![
                SecretSlot {
                    member: "apiKeySecret",
                    value_member: "apiKeySecret",
                    value: CDP_API_KEY_SECRET,
                    json_key: None,
                    source_sent: false,
                },
                SecretSlot {
                    member: "walletSecret",
                    value_member: "walletSecret",
                    value: "d2FsbGV0LXNlY3JldA==",
                    json_key: None,
                    source_sent: true,
                },
            ],
        },
        Row {
            case: "a payment provider with one referenced secret and one the service manages",
            plane: Plane::Payment,
            name: "payment_mixed",
            region: "eu-central-1",
            slots: vec![
                SecretSlot {
                    member: "apiKeySecret",
                    value_member: "apiKeySecret",
                    value: "ZXh0ZXJuYWwtYXBpLWtleQ==",
                    json_key: Some("cdp-key"),
                    source_sent: true,
                },
                SecretSlot {
                    member: "walletSecret",
                    value_member: "walletSecret",
                    value: "bWl4ZWQtd2FsbGV0",
                    json_key: None,
                    source_sent: false,
                },
            ],
        },
    ]
}

/// Every credential provider is addressed by a token-vault ARN in this service's
/// own ARN namespace, and reports a secret's JSON key only where the request named
/// one — on the create and on every later read alike.
#[tokio::test]
async fn a_credential_provider_is_represented_by_its_arn_and_the_json_key_the_caller_named() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let request = create_request(&emulator, &row).await;

        let (status, created) = emulator
            .control_plane(row.plane.create_uri(), row.region, &request)
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from {}, got {status} ({created})",
            row.plane.create_uri()
        );
        assert_representation(case, &created, &row);

        // The round trip: the read answers from the record the create persisted, so
        // it reports the one representation.
        let (status, fetched) = emulator
            .control_plane(
                row.plane.get_uri(),
                row.region,
                &json!({"name": row.name}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {}, got {status} ({fetched})",
            row.plane.get_uri()
        );
        assert_representation(case, &fetched, &row);
        for slot in &row.slots {
            for pointer in [
                row.plane.json_key_pointer(slot.member),
                row.plane.source_pointer(slot.member),
            ] {
                assert_eq!(
                    fetched.pointer(&pointer).cloned(),
                    created.pointer(&pointer).cloned(),
                    "case {case:?}: the read disagrees with the create about {pointer:?}, got \
                     {fetched}"
                );
            }
        }
        assert_eq!(
            fetched["credentialProviderArn"], created["credentialProviderArn"],
            "case {case:?}: the read disagrees with the create about the provider's ARN, got \
             {fetched}"
        );
    }
}
