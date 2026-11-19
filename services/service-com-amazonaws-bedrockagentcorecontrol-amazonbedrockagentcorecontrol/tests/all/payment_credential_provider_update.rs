// SPDX-License-Identifier: CC0-1.0

//! `UpdatePaymentCredentialProvider`, and the read that reports what it wrote.
//!
//! The rule under test: an update addresses an EXISTING payment credential
//! provider by `name` — a name this account holds no provider under is the
//! modelled 404, never a provider an update mints — and replaces the whole
//! vendor configuration the provider authenticates with by the one the request
//! carries, storing each of that vendor's two secrets where its `*Source` says:
//! the SERVICE's own Secrets Manager secret for `MANAGED`, holding the new value
//! the request handed over, and the CALLER's own secret for `EXTERNAL`, written
//! to by nobody. Each secret is reported by the ARN Secrets Manager holds it at
//! and never by its value. What identifies the provider rather than configures
//! it — the name, the ARN minted from it, the tags it carries and the instant it
//! was created — is untouched, `lastUpdatedTime` is the instant of the update,
//! and a later read answers from that one record.
//!
//! A refused update changes nothing: the provider and the secrets it named stand
//! exactly as they were.
//!
//! The rows span what the rule generalises over: the vendor the provider carries
//! before and after (each one re-configured, and each switch between the two,
//! which is also a switch of the union member the credentials are carried in),
//! every transition between the two secret sources (managed to managed, managed
//! to external, external to managed, external to external), whether the request
//! spells `*Source` out or leaves it to the rule, whether an external secret is
//! referenced by name or by its whole ARN, the provider's name (the shortest and
//! the longest `CredentialProviderName` admits) and the Region the request is
//! signed for. The recorded case is one row.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region the rows are signed for unless one names another.
const REGION: &str = "us-east-1";

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The token vault a request naming none addresses, which a provider's ARN nests
/// it under.
const VAULT: &str = "default";

/// `PaymentCredentialProviderVendorType`'s full value list, `CoinbaseCDP |
/// StripePrivy` (`smithy_query(query="shape",
/// shape="PaymentCredentialProviderVendorType")`).
const VENDOR_COINBASE_CDP: &str = "CoinbaseCDP";
const VENDOR_STRIPE_PRIVY: &str = "StripePrivy";

/// The `PaymentProviderConfigurationInput` / `…Output` member each vendor's
/// configuration is carried in (`smithy_query(query="shape",
/// shape="PaymentProviderConfigurationInput")`).
const COINBASE_MEMBER: &str = "coinbaseCdpConfiguration";
const PRIVY_MEMBER: &str = "stripePrivyConfiguration";

/// `SecretSourceType`'s full value list, `MANAGED | EXTERNAL`
/// (`smithy_query(query="shape", shape="SecretSourceType")`).
const SOURCE_MANAGED: &str = "MANAGED";
const SOURCE_EXTERNAL: &str = "EXTERNAL";

/// The longest name `CredentialProviderName` admits: length 1..128, pattern
/// `^[a-zA-Z0-9\-_]+$` (`smithy_query(query="operation",
/// operation="UpdatePaymentCredentialProvider")`).
const MAX_PROVIDER_NAME_LEN: usize = 128;

/// The base64 of `length` bytes of an Ed25519 private key: a stand-in for "the
/// API key secret provided by Coinbase Developer Platform"
/// (`smithy_query(query="doc", shape="CoinbaseCdpConfigurationInput",
/// member="apiKeySecret")`), which that vendor issues as the 32-byte seed
/// followed by the public key it derives — [`ISSUED_KEY_BYTES`]. A `MANAGED`
/// value that is not such a key is refused, so every row handing one over sends
/// one of these rather than an arbitrary string.
fn cdp_api_key_secret(filler: u8, length: usize) -> String {
    STANDARD.encode(vec![filler; length])
}

/// The byte length the Coinbase CDP API key secret is issued in: the seed and
/// the public key it derives.
const ISSUED_KEY_BYTES: usize = 64;

/// The Coinbase CDP API key secret the provider a refused update must be left
/// holding was created with, so the refusal rows also assert the stored secret
/// still carries it.
fn original_api_key_secret() -> String {
    cdp_api_key_secret(b'o', ISSUED_KEY_BYTES)
}

/// The name the secret holding ONE managed secret of a payment provider is
/// created under: the service's own prefix, the vault, the resource type, the
/// provider and the member the secret holds. Spelled out here rather than read
/// off the implementation, so a drift in either is a failing test.
fn managed_secret_name(provider: &str, member: &str) -> String {
    format!("bedrock-agentcore-identity/{VAULT}/paymentcredentialprovider/{provider}/{member}")
}

/// The ARN a payment credential provider named `name` carries in `region`: the
/// token-vault path of `PaymentCredentialProviderArnType`, in the ARN namespace
/// of the service that holds the vault.
fn provider_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:token-vault/{VAULT}/\
         paymentcredentialprovider/{name}"
    )
}

/// The prefix the ARN of a secret named `name` carries in `region`: a created
/// secret's ARN ends in six characters that belong to the secret rather than to
/// its name.
fn secret_arn_prefix(region: &str, name: &str) -> String {
    format!("arn:aws:secretsmanager:{region}:{ACCOUNT}:secret:{name}-")
}

/// A UUID-shaped `ClientRequestToken` derived from a secret's name — Secrets
/// Manager stakes the version a create stores on one the caller names, 32..64
/// characters.
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

/// The emulator the rows run against: the control plane, and Secrets Manager
/// beside it on the one shared engine — what the assembled emulator is
/// (`core/router`'s `build_router` registers every service crate), and what a
/// provider's secret ARNs have to name real secrets in.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        // `default()` registers no authenticator, so the suite exercises routing
        // and the handlers rather than this process's SigV4 keys.
        let mut router = MultiServiceRouter::default();
        router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// One `restJson1` request to the control plane, signed for `region` — which
    /// is what the router routes on and what the handlers read the Region from.
    async fn control(&self, uri: &str, region: &str, body: &Value) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/bedrock-agentcore/\
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

    async fn create_provider(&self, region: &str, body: &Value) -> Answer {
        self.control("/identities/CreatePaymentCredentialProvider", region, body)
            .await
    }

    async fn update_provider(&self, region: &str, body: &Value) -> Answer {
        self.control("/identities/UpdatePaymentCredentialProvider", region, body)
            .await
    }

    async fn get_provider(&self, region: &str, name: &str) -> Answer {
        self.control(
            "/identities/GetPaymentCredentialProvider",
            region,
            &json!({"name": name}),
        )
        .await
    }

    /// One Secrets Manager call (`awsJson1_1`, `secretsmanager.<Operation>`)
    /// signed for `region`. The credential scope names `secretsmanager`, which
    /// is what the router claims on.
    async fn secrets(&self, region: &str, operation: &str, body: &Value) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/secretsmanager/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{region}.amazonaws.com/"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("secretsmanager.{operation}"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// The value Secrets Manager holds `secret_id` at, as the JSON document a
    /// secret's value is.
    async fn secret_document(&self, case: &str, region: &str, secret_id: &str) -> Value {
        let answer = self
            .secrets(region, "GetSecretValue", &json!({"SecretId": secret_id}))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from GetSecretValue for {secret_id:?}, got {} and \
             body {}",
            answer.status, answer.body
        );
        let text = answer.body["SecretString"].as_str().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a SecretString for {secret_id:?}, got body {}",
                answer.body
            )
        });
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }

    /// Create a secret of the CALLER's own, as an external configuration
    /// references one, and report the ARN Secrets Manager holds it at.
    async fn put_external_secret(
        &self,
        case: &str,
        region: &str,
        name: &str,
        value: &Value,
    ) -> String {
        let answer = self
            .secrets(
                region,
                "CreateSecret",
                &json!({
                    "Name": name,
                    "SecretString": value.to_string(),
                    "ClientRequestToken": client_request_token(name),
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from CreateSecret for {name:?}, got {} and body {}",
            answer.status, answer.body
        );
        answer.body["ARN"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a created secret reports its ARN, got body {}",
                    answer.body
                )
            })
            .to_string()
    }

    /// Every secret name Secrets Manager holds in `region`.
    async fn secret_names(&self, case: &str, region: &str) -> Vec<String> {
        let answer = self.secrets(region, "ListSecrets", &json!({})).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from ListSecrets, got {} and body {}",
            answer.status, answer.body
        );
        let mut names: Vec<String> = answer.body["SecretList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|secret| secret["Name"].as_str().map(str::to_string))
            .collect();
        names.sort();
        names
    }
}

/// One answer from the emulator: its status and its JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Self { status, body }
    }

    /// The error code `restJson1` and `awsJson1_1` both carry in `__type`.
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

    /// An epoch-seconds instant off a response member. The prelude `Timestamp`
    /// carries no `@timestampFormat`, so on this service's restJson1 wire each
    /// instant is a NUMBER.
    fn instant(&self, case: &str, member: &str) -> f64 {
        self.body[member].as_f64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {member} as epoch-seconds, got body {}",
                self.body
            )
        })
    }
}

/// Where ONE secret of a vendor configuration lives, as a request describes it.
#[derive(Debug, Clone)]
enum Keeping {
    /// The service manages the secret: `value` is what goes into it. `named` is
    /// whether the request spells the `*Source` member out or leaves it to the
    /// rule.
    Managed { value: String, named: bool },
    /// The caller manages the secret: it is created before the request is made,
    /// under `secret_name` holding `value` at `json_key`. `named` is whether the
    /// request spells the source out or lets the reference imply it, and
    /// `by_arn` whether it is referenced by its whole ARN rather than by name.
    External {
        secret_name: String,
        value: String,
        json_key: String,
        named: bool,
        by_arn: bool,
    },
}

impl Keeping {
    fn managed(value: &str, named: bool) -> Self {
        Self::Managed {
            value: value.to_string(),
            named,
        }
    }

    fn external(secret_name: &str, value: &str, json_key: &str, named: bool, by_arn: bool) -> Self {
        Self::External {
            secret_name: secret_name.to_string(),
            value: value.to_string(),
            json_key: json_key.to_string(),
            named,
            by_arn,
        }
    }

    /// The `*Source` a secret described this way ends up with.
    fn effective(&self) -> &'static str {
        match self {
            Self::Managed { .. } => SOURCE_MANAGED,
            Self::External { .. } => SOURCE_EXTERNAL,
        }
    }

    /// The secret value itself, which no response may carry.
    fn value(&self) -> &str {
        match self {
            Self::Managed { value, .. } | Self::External { value, .. } => value,
        }
    }
}

/// One secret of a vendor configuration: the member it is sent as — which is
/// also the prefix of the three output members reporting it (`…Arn`,
/// `…JsonKey`, `…Source`) — and where it lives.
#[derive(Debug, Clone)]
struct Slot {
    member: &'static str,
    keeping: Keeping,
}

/// One whole vendor configuration a request carries: the vendor, the union
/// member its credentials are carried in, the vendor's non-secret identifiers
/// and its two secrets.
#[derive(Debug, Clone)]
struct Configuration {
    vendor: &'static str,
    union_member: &'static str,
    identifiers: Value,
    slots: [Slot; 2],
}

impl Configuration {
    fn coinbase(api_key_id: &str, api_key: Keeping, wallet: Keeping) -> Self {
        Self {
            vendor: VENDOR_COINBASE_CDP,
            union_member: COINBASE_MEMBER,
            identifiers: json!({"apiKeyId": api_key_id}),
            slots: [
                Slot {
                    member: "apiKeySecret",
                    keeping: api_key,
                },
                Slot {
                    member: "walletSecret",
                    keeping: wallet,
                },
            ],
        }
    }

    fn privy(app_id: &str, authorization_id: &str, app: Keeping, private_key: Keeping) -> Self {
        Self {
            vendor: VENDOR_STRIPE_PRIVY,
            union_member: PRIVY_MEMBER,
            identifiers: json!({"appId": app_id, "authorizationId": authorization_id}),
            slots: [
                Slot {
                    member: "appSecret",
                    keeping: app,
                },
                Slot {
                    member: "authorizationPrivateKey",
                    keeping: private_key,
                },
            ],
        }
    }

    /// The `providerConfigurationInput` a request carries for this
    /// configuration, with the caller's own secrets put into Secrets Manager
    /// first — whose ARNs are reported back so a row can assert the provider
    /// reports exactly them.
    async fn request_members(
        &self,
        emulator: &Emulator,
        case: &str,
        region: &str,
        suffix: &str,
    ) -> (Value, [Option<String>; 2]) {
        let mut configuration = self.identifiers.clone();
        let mut owners_arns: [Option<String>; 2] = [None, None];
        for (index, slot) in self.slots.iter().enumerate() {
            match &slot.keeping {
                Keeping::Managed { value, named } => {
                    configuration[slot.member] = json!(value);
                    if *named {
                        configuration[format!("{}Source", slot.member)] = json!(SOURCE_MANAGED);
                    }
                }
                Keeping::External {
                    secret_name,
                    value,
                    json_key,
                    named,
                    by_arn,
                } => {
                    let name = format!("{secret_name}/{suffix}");
                    let owners_arn = emulator
                        .put_external_secret(case, region, &name, &json!({ json_key: value }))
                        .await;
                    let referenced = if *by_arn { owners_arn.clone() } else { name };
                    configuration[format!("{}Config", slot.member)] =
                        json!({"secretId": referenced, "jsonKey": json_key});
                    if *named {
                        configuration[format!("{}Source", slot.member)] = json!(SOURCE_EXTERNAL);
                    }
                    owners_arns[index] = Some(owners_arn);
                }
            }
        }
        (json!({ self.union_member: configuration }), owners_arns)
    }
}

/// One provider a row creates and then updates.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    name: String,
    region: &'static str,
    /// The configuration the create carried.
    before: Configuration,
    /// The configuration the update carries.
    after: Configuration,
}

/// The updates the rule is asserted over: one row per thing it generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "Coinbase CDP re-keyed, both secrets managed, the source left to the rule then \
                   named",
            name: "a".to_string(),
            region: REGION,
            before: Configuration::coinbase(
                "cdp-api-key-1",
                Keeping::managed(&cdp_api_key_secret(b'a', ISSUED_KEY_BYTES), false),
                Keeping::managed("Zmlyc3Qtd2FsbGV0", false),
            ),
            after: Configuration::coinbase(
                "cdp-api-key-1-rotated",
                Keeping::managed(&cdp_api_key_secret(b'b', ISSUED_KEY_BYTES), true),
                Keeping::managed("c2Vjb25kLXdhbGxldA==", true),
            ),
        },
        Row {
            case: "a Coinbase CDP provider's secrets handed over to the caller's own, one \
                   referenced by its whole ARN",
            name: "partner-payments_2026".to_string(),
            region: REGION,
            before: Configuration::coinbase(
                "cdp-api-key-2",
                Keeping::managed(&cdp_api_key_secret(b'c', ISSUED_KEY_BYTES), true),
                Keeping::managed("bWFuYWdlZC13YWxsZXQ=", false),
            ),
            after: Configuration::coinbase(
                "cdp-api-key-2",
                Keeping::external(
                    "caller/cdp/api-key",
                    "ZXh0ZXJuYWwtYXBpLWtleQ==",
                    "apiKeySecret",
                    true,
                    false,
                ),
                Keeping::external(
                    "caller/cdp/wallet",
                    "ZXh0ZXJuYWwtd2FsbGV0",
                    "wallet",
                    false,
                    true,
                ),
            ),
        },
        Row {
            case: "a Coinbase CDP provider's caller-managed secrets taken over by the service, in \
                   a non-default Region",
            name: "vendor_handover".to_string(),
            region: "eu-west-2",
            before: Configuration::coinbase(
                "cdp-api-key-3",
                Keeping::external(
                    "caller/handover/api-key",
                    "Y2FsbGVyLWFwaS1rZXk=",
                    "apiKeySecret",
                    true,
                    false,
                ),
                Keeping::external(
                    "caller/handover/wallet",
                    "Y2FsbGVyLXdhbGxldA==",
                    "wallet",
                    true,
                    false,
                ),
            ),
            after: Configuration::coinbase(
                "cdp-api-key-3",
                Keeping::managed(&cdp_api_key_secret(b'd', ISSUED_KEY_BYTES), false),
                Keeping::managed("c2VydmljZS13YWxsZXQ=", true),
            ),
        },
        Row {
            case: "the vendor switched from Coinbase CDP to Stripe Privy, under the longest name \
                   a provider admits",
            name: format!("L{}", "o".repeat(MAX_PROVIDER_NAME_LEN - 1)),
            region: "ap-southeast-2",
            before: Configuration::coinbase(
                "cdp-api-key-4",
                Keeping::managed(&cdp_api_key_secret(b'e', ISSUED_KEY_BYTES), false),
                Keeping::managed("Y29pbmJhc2Utd2FsbGV0", false),
            ),
            after: Configuration::privy(
                "privy-app-4",
                "privy-authorization-4",
                Keeping::managed("cHJpdnktYXBwLXNlY3JldA==", true),
                // `DefaultStripePrivyAuthorizationPrivateKeyType`'s pattern
                // admits a `wallet-auth:` prefix (`smithy_query(query="shape",
                // shape="StripePrivyConfigurationInput")`).
                Keeping::managed("wallet-auth:cHJpdmF0ZS1rZXk=", false),
            ),
        },
        Row {
            case: "the vendor switched from Stripe Privy to Coinbase CDP, one secret staying with \
                   the caller",
            name: "switched_back".to_string(),
            region: REGION,
            before: Configuration::privy(
                "privy-app-5",
                "privy-authorization-5",
                Keeping::external(
                    "caller/privy/app",
                    "Y2FsbGVyLWFwcC1zZWNyZXQ=",
                    "appSecret",
                    true,
                    false,
                ),
                Keeping::managed("cHJpdnktcHJpdmF0ZS1rZXk=", false),
            ),
            after: Configuration::coinbase(
                "cdp-api-key-5",
                Keeping::external(
                    "caller/cdp/switched",
                    "c3dpdGNoZWQtYXBpLWtleQ==",
                    "key",
                    false,
                    false,
                ),
                Keeping::managed("c3dpdGNoZWQtd2FsbGV0", true),
            ),
        },
        Row {
            case: "a Stripe Privy provider's caller-managed secrets swapped for other \
                   caller-managed ones",
            name: "privy-external-swap".to_string(),
            region: "ap-northeast-1",
            before: Configuration::privy(
                "privy-app-6",
                "privy-authorization-6",
                Keeping::external(
                    "caller/first/app",
                    "Zmlyc3QtYXBwLXNlY3JldA==",
                    "appSecret",
                    true,
                    false,
                ),
                Keeping::external(
                    "caller/first/key",
                    "Zmlyc3QtcHJpdmF0ZS1rZXk=",
                    "privateKey",
                    false,
                    false,
                ),
            ),
            after: Configuration::privy(
                "privy-app-6-rotated",
                "privy-authorization-6-rotated",
                Keeping::external(
                    "caller/second/app",
                    "c2Vjb25kLWFwcC1zZWNyZXQ=",
                    "secret",
                    false,
                    true,
                ),
                Keeping::external(
                    "caller/second/key",
                    "c2Vjb25kLXByaXZhdGUta2V5",
                    "authorizationPrivateKey",
                    true,
                    false,
                ),
            ),
        },
    ]
}

/// Assert the configuration `body` reports is the one `configuration` describes
/// — the vendor's union member, its identifiers, and per secret the effective
/// source, the ARN its owner holds it at and the JSON key its value is extracted
/// under — and report the secret ARN each slot named.
fn assert_configuration(
    case: &str,
    body: &Value,
    configuration: &Configuration,
    region: &str,
    provider: &str,
    owners_arns: &[Option<String>; 2],
) -> [String; 2] {
    assert_eq!(
        body["credentialProviderVendor"],
        json!(configuration.vendor),
        "case {case:?}: expected the vendor the configuration belongs to, got body {body}"
    );
    let reported = &body["providerConfigurationOutput"];
    let members: Vec<&String> = reported
        .as_object()
        .map(|members| members.keys().collect())
        .unwrap_or_default();
    assert_eq!(
        members,
        vec![&configuration.union_member.to_string()],
        "case {case:?}: the configuration is carried in the one member its vendor requires, got \
         body {body}"
    );
    let reported = &reported[configuration.union_member];
    for (identifier, expected) in configuration
        .identifiers
        .as_object()
        .expect("a vendor's identifiers are a JSON object")
    {
        assert_eq!(
            &reported[identifier], expected,
            "case {case:?}: expected {identifier} as sent, got body {body}"
        );
    }

    let mut arns = [String::new(), String::new()];
    for (index, slot) in configuration.slots.iter().enumerate() {
        let member = slot.member;
        assert_eq!(
            reported[format!("{member}Source")],
            json!(slot.keeping.effective()),
            "case {case:?} ({member}): expected the effective secret source, got body {body}"
        );
        let secret_arn = reported[format!("{member}Arn")]["secretArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?} ({member}): expected a secret ARN, got body {body}")
            })
            .to_string();
        match (&slot.keeping, &owners_arns[index]) {
            (Keeping::Managed { .. }, _) => {
                let prefix = secret_arn_prefix(region, &managed_secret_name(provider, member));
                assert!(
                    secret_arn.starts_with(&prefix) && secret_arn.len() > prefix.len(),
                    "case {case:?} ({member}): a managed secret lives in the secret derived for \
                     it, expected an ARN under {prefix:?}, got {secret_arn:?}"
                );
                assert_eq!(
                    reported[format!("{member}JsonKey")],
                    Value::Null,
                    "case {case:?} ({member}): a secret the service manages is reported with no \
                     JSON key, got body {body}"
                );
            }
            (Keeping::External { json_key, .. }, Some(owners_arn)) => {
                assert_eq!(
                    &secret_arn, owners_arn,
                    "case {case:?} ({member}): an external secret is reported by the ARN Secrets \
                     Manager holds it at, got {secret_arn:?}"
                );
                assert_eq!(
                    reported[format!("{member}JsonKey")],
                    json!(json_key),
                    "case {case:?} ({member}): expected the caller's jsonKey, got body {body}"
                );
            }
            (Keeping::External { .. }, None) => panic!(
                "case {case:?} ({member}): an external secret must have been created for this \
                 request"
            ),
        }
        arns[index] = secret_arn;
    }
    arns
}

/// Assert each secret the configuration describes really holds what the request
/// asked of it: the new value under the member it was sent as for a secret the
/// service manages, and the caller's own document untouched for one it does not.
async fn assert_secret_contents(
    emulator: &Emulator,
    case: &str,
    region: &str,
    configuration: &Configuration,
    arns: &[String; 2],
) {
    for (index, slot) in configuration.slots.iter().enumerate() {
        let document = emulator.secret_document(case, region, &arns[index]).await;
        match &slot.keeping {
            Keeping::Managed { value, .. } => {
                assert_eq!(
                    document[slot.member],
                    json!(value),
                    "case {case:?} ({}): the managed secret holds the value the request handed \
                     over, got {document}",
                    slot.member
                );
            }
            Keeping::External {
                value, json_key, ..
            } => {
                assert_eq!(
                    document,
                    json!({ json_key.as_str(): value }),
                    "case {case:?} ({}): an external secret is left exactly as the caller stored \
                     it, got {document}",
                    slot.member
                );
            }
        }
    }
}

/// A served update replaces the provider's whole vendor configuration as the
/// request describes it, leaves what identifies the provider alone, and a later
/// read answers from that one record.
#[tokio::test]
async fn an_updated_provider_reads_back_with_its_new_configuration() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();

        let (before_configuration, before_arns) = row
            .before
            .request_members(&emulator, case, row.region, "before")
            .await;
        let created = emulator
            .create_provider(
                row.region,
                &json!({
                    "name": row.name,
                    "credentialProviderVendor": row.before.vendor,
                    "providerConfigurationInput": before_configuration,
                    "tags": {"team": "payments"},
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201 from CreatePaymentCredentialProvider, got {} and \
             body {}",
            created.status, created.body
        );
        let created_arns = assert_configuration(
            case,
            &created.body,
            &row.before,
            row.region,
            &row.name,
            &before_arns,
        );
        let before_read = emulator.get_provider(row.region, &row.name).await;
        let created_time = before_read.instant(case, "createdTime");

        let (after_configuration, after_arns) = row
            .after
            .request_members(&emulator, case, row.region, "after")
            .await;
        let updated = emulator
            .update_provider(
                row.region,
                &json!({
                    "name": row.name,
                    "credentialProviderVendor": row.after.vendor,
                    "providerConfigurationInput": after_configuration,
                }),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected HTTP 200 from UpdatePaymentCredentialProvider, got {} and \
             body {}",
            updated.status, updated.body
        );

        // What identifies the provider is untouched by an update that
        // configures it.
        assert_eq!(
            updated.body["name"],
            json!(row.name),
            "case {case:?}: an update reports the provider's own name, got body {}",
            updated.body
        );
        assert_eq!(
            updated.body["credentialProviderArn"],
            json!(provider_arn(row.region, &row.name)),
            "case {case:?}: an update leaves the provider's ARN as it was, got body {}",
            updated.body
        );
        assert_eq!(
            updated.instant(case, "createdTime"),
            created_time,
            "case {case:?}: an update leaves the instant the provider was created alone, got body \
             {}",
            updated.body
        );
        assert!(
            updated.instant(case, "lastUpdatedTime") >= created_time,
            "case {case:?}: expected lastUpdatedTime at or after createdTime {created_time}, got \
             {}",
            updated.instant(case, "lastUpdatedTime")
        );

        // The configuration the update reports is the one the request asked
        // for, and each of its secrets really holds what the request asked of
        // it.
        let updated_arns = assert_configuration(
            case,
            &updated.body,
            &row.after,
            row.region,
            &row.name,
            &after_arns,
        );
        assert_secret_contents(&emulator, case, row.region, &row.after, &updated_arns).await;
        for (index, slot) in row.after.slots.iter().enumerate() {
            let same_slot = row.before.slots[index].member == slot.member;
            if same_slot
                && matches!(row.before.slots[index].keeping, Keeping::Managed { .. })
                && matches!(slot.keeping, Keeping::Managed { .. })
            {
                assert_eq!(
                    updated_arns[index], created_arns[index],
                    "case {case:?} ({}): the secret the service manages for one member of one \
                     provider is the one secret, so a replaced value stays in it",
                    slot.member
                );
            }
        }

        // No response carries a secret VALUE: the output configuration is "no
        // raw secrets, only ARNs".
        let bodies = format!("{}{}", created.body, updated.body);
        for slot in row.before.slots.iter().chain(row.after.slots.iter()) {
            assert!(
                !bodies.contains(slot.keeping.value()),
                "case {case:?} ({}): a response carried a secret value",
                slot.member
            );
        }

        // The round trip: the read reports the one record the update wrote,
        // together with the tags and timestamps only it declares.
        let read = emulator.get_provider(row.region, &row.name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetPaymentCredentialProvider, got {} and body \
             {}",
            read.status, read.body
        );
        for member in [
            "name",
            "credentialProviderVendor",
            "credentialProviderArn",
            "providerConfigurationOutput",
            "createdTime",
            "lastUpdatedTime",
        ] {
            assert_eq!(
                read.body[member], updated.body[member],
                "case {case:?}: the read reports the same {member} as the update, got body {}",
                read.body
            );
        }
        assert_eq!(
            read.body["tags"],
            json!({"team": "payments"}),
            "case {case:?}: an update leaves the tags the create carried alone, got body {}",
            read.body
        );
    }
}

/// A Coinbase CDP configuration whose two secrets the service manages, the API
/// key secret in the format that vendor issues one in.
fn coinbase_managed(api_key: &str, wallet: &str) -> Value {
    json!({
        "coinbaseCdpConfiguration": {
            "apiKeyId": "cdp-api-key",
            "apiKeySecret": api_key,
            "walletSecret": wallet,
        }
    })
}

/// The provider an update names has to exist: a name this account holds no
/// provider under is the modelled 404, and the update mints nothing — no
/// provider, and no secret for one.
#[tokio::test]
async fn an_update_of_an_unknown_name_is_not_found() {
    let emulator = Emulator::new();
    let held = "stored_provider";
    let created = emulator
        .create_provider(
            REGION,
            &json!({
                "name": held,
                "credentialProviderVendor": VENDOR_COINBASE_CDP,
                "providerConfigurationInput":
                    coinbase_managed(
                        &cdp_api_key_secret(b's', ISSUED_KEY_BYTES),
                        "c3RvcmVkLXdhbGxldA==",
                    ),
            }),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "the stored provider must be there, got {} and body {}",
        created.status, created.body
    );

    for (case, name) in [
        ("nothing was ever created under it", "absent_provider"),
        (
            "a name differing only in case from one that exists",
            "Stored_Provider",
        ),
    ] {
        let refused = emulator
            .update_provider(
                REGION,
                &json!({
                    "name": name,
                    "credentialProviderVendor": VENDOR_COINBASE_CDP,
                    "providerConfigurationInput":
                        coinbase_managed(
                            &cdp_api_key_secret(b'n', ISSUED_KEY_BYTES),
                            "bmV3LXdhbGxldA==",
                        ),
                }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: expected the modelled 404 for {name:?}, got {} and body {}",
            refused.status,
            refused.body
        );
        // The refusal names the resource type and the member the request
        // addressed the provider by, which is what the caller corrects.
        assert_eq!(
            refused.message(),
            format!("PaymentCredentialProvider not found for name: {name}"),
            "case {case:?}: unexpected refusal wording for {name:?}"
        );
        let read = emulator.get_provider(REGION, name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused update records no provider, got {} and body {}",
            read.status, read.body
        );
    }

    // Exactly the two secrets the create minted stand: a refused update stored
    // nothing for a provider that does not exist.
    let mut expected = vec![
        managed_secret_name(held, "apiKeySecret"),
        managed_secret_name(held, "walletSecret"),
    ];
    expected.sort();
    let names = emulator.secret_names("a refused update", REGION).await;
    assert_eq!(
        names, expected,
        "a refused update may mint no secret, got {names:?}"
    );
}

/// The rules the model states in prose hold for an update as they do for a
/// create — the vendor decides which member of the union carries the
/// credentials, an external secret needs the reference it is external to, and a
/// managed one needs the value the service stores — and a refused update leaves
/// the provider exactly as it was.
#[tokio::test]
async fn an_update_whose_configuration_breaks_a_rule_is_refused() {
    let name = "guarded_provider";
    for (case, member, vendor, configuration) in [
        (
            "a Stripe Privy configuration sent for the Coinbase CDP vendor",
            "providerConfigurationInput",
            VENDOR_COINBASE_CDP,
            json!({
                "stripePrivyConfiguration": {
                    "appId": "privy-app",
                    "authorizationId": "privy-authorization",
                    "appSecret": "cHJpdnktYXBw",
                    "authorizationPrivateKey": "cHJpdnkta2V5",
                }
            }),
        ),
        (
            "a Coinbase CDP configuration sent for the Stripe Privy vendor",
            "providerConfigurationInput",
            VENDOR_STRIPE_PRIVY,
            coinbase_managed(&cdp_api_key_secret(b'p', ISSUED_KEY_BYTES), "Y2RwLXdhbGxldA=="),
        ),
        (
            "EXTERNAL with no secret reference",
            "walletSecretConfig",
            VENDOR_COINBASE_CDP,
            json!({
                "coinbaseCdpConfiguration": {
                    "apiKeyId": "cdp-api-key",
                    "apiKeySecret": cdp_api_key_secret(b'p', ISSUED_KEY_BYTES),
                    "walletSecretSource": SOURCE_EXTERNAL,
                }
            }),
        ),
        (
            "MANAGED with no value at all",
            "apiKeySecret",
            VENDOR_COINBASE_CDP,
            json!({
                "coinbaseCdpConfiguration": {
                    "apiKeyId": "cdp-api-key",
                    "apiKeySecretSource": SOURCE_MANAGED,
                    "walletSecret": "Y2RwLXdhbGxldA==",
                }
            }),
        ),
        (
            "the default source with an empty value",
            "authorizationPrivateKey",
            VENDOR_STRIPE_PRIVY,
            json!({
                "stripePrivyConfiguration": {
                    "appId": "privy-app",
                    "authorizationId": "privy-authorization",
                    "appSecret": "cHJpdnktYXBw",
                    "authorizationPrivateKey": "",
                }
            }),
        ),
        (
            "a MANAGED Coinbase CDP API key secret that is not a key in the format that vendor \
             issues",
            "apiKeySecret",
            VENDOR_COINBASE_CDP,
            // One byte short of the key the vendor issues: inside the member's
            // own pattern, and not a key.
            coinbase_managed(
                &cdp_api_key_secret(b'q', ISSUED_KEY_BYTES - 1),
                "Y2RwLXdhbGxldA==",
            ),
        ),
        (
            "a secret of another account referenced as the caller's own",
            "apiKeySecretConfig.secretId",
            VENDOR_COINBASE_CDP,
            json!({
                "coinbaseCdpConfiguration": {
                    "apiKeyId": "cdp-api-key",
                    "apiKeySecretConfig": {
                        "secretId":
                            "arn:aws:secretsmanager:us-east-1:210987654321:secret:other-aaaaaa",
                        "jsonKey": "apiKeySecret",
                    },
                    "walletSecret": "Y2RwLXdhbGxldA==",
                }
            }),
        ),
    ] {
        let emulator = Emulator::new();
        let created = emulator
            .create_provider(
                REGION,
                &json!({
                    "name": name,
                    "credentialProviderVendor": VENDOR_COINBASE_CDP,
                    "providerConfigurationInput":
                        coinbase_managed(&original_api_key_secret(), "b3JpZ2luYWwtd2FsbGV0"),
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: the provider to update must be there, got {} and body {}",
            created.status, created.body
        );
        let before = emulator.get_provider(REGION, name).await;

        let refused = emulator
            .update_provider(
                REGION,
                &json!({
                    "name": name,
                    "credentialProviderVendor": vendor,
                    "providerConfigurationInput": configuration,
                }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "ValidationException".to_string()),
            "case {case:?}: expected the modelled 400, got {} and body {}",
            refused.status,
            refused.body
        );
        let message = refused.message();
        assert!(
            message.contains(member),
            "case {case:?}: the refusal must name {member:?}, got {message:?}"
        );

        // Nothing changed: the provider reads back exactly as it was, and its
        // managed secrets still hold what the create stored.
        let after = emulator.get_provider(REGION, name).await;
        assert_eq!(
            after.body, before.body,
            "case {case:?}: a refused update may not change the provider"
        );
        let original_api_key = original_api_key_secret();
        for (member, value) in [
            ("apiKeySecret", original_api_key.as_str()),
            ("walletSecret", "b3JpZ2luYWwtd2FsbGV0"),
        ] {
            let document = emulator
                .secret_document(case, REGION, &managed_secret_name(name, member))
                .await;
            assert_eq!(
                document[member],
                json!(value),
                "case {case:?} ({member}): a refused update may not replace a stored secret, got \
                 {document}"
            );
        }
    }
}

/// An update pointing a provider at a secret nobody holds is refused, and the
/// provider keeps the configuration it had: every ARN its answer would carry has
/// to name a secret Secrets Manager really holds.
#[tokio::test]
async fn an_update_to_an_external_secret_that_is_not_there_is_refused() {
    let name = "external_handover";
    for (case, secret_id) in [
        ("a secret named by name", "no/such/secret"),
        (
            "a secret named by ARN",
            "arn:aws:secretsmanager:us-east-1:000000000000:secret:no-such-secret-aaaaaa",
        ),
    ] {
        let emulator = Emulator::new();
        let created = emulator
            .create_provider(
                REGION,
                &json!({
                    "name": name,
                    "credentialProviderVendor": VENDOR_COINBASE_CDP,
                    "providerConfigurationInput":
                        coinbase_managed(&original_api_key_secret(), "b3JpZ2luYWwtd2FsbGV0"),
                }),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: the provider to update must be there, got {} and body {}",
            created.status, created.body
        );
        let before = emulator.get_provider(REGION, name).await;

        let refused = emulator
            .update_provider(
                REGION,
                &json!({
                    "name": name,
                    "credentialProviderVendor": VENDOR_COINBASE_CDP,
                    "providerConfigurationInput": {
                        "coinbaseCdpConfiguration": {
                            "apiKeyId": "cdp-api-key",
                            "apiKeySecretSource": SOURCE_EXTERNAL,
                            "apiKeySecretConfig": {
                                "secretId": secret_id,
                                "jsonKey": "apiKeySecret",
                            },
                            "walletSecret": "bmV3LXdhbGxldA==",
                        }
                    },
                }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: expected the modelled 404 for {secret_id:?}, got {} and body {}",
            refused.status,
            refused.body
        );

        let after = emulator.get_provider(REGION, name).await;
        assert_eq!(
            after.body, before.body,
            "case {case:?}: a provider whose new secrets cannot be resolved is left as it was"
        );

        // The secret the service manages for the provider holds what the create
        // put there: the refused update's new `walletSecret` was never stored,
        // because the configuration is resolved in the order its members are
        // declared and the reference that names no secret comes first.
        let document = emulator
            .secret_document(case, REGION, &managed_secret_name(name, "walletSecret"))
            .await;
        assert_eq!(
            document["walletSecret"],
            json!("b3JpZ2luYWwtd2FsbGV0"),
            "case {case:?}: a refused update may not replace a stored secret, got {document}"
        );
    }
}
