// SPDX-License-Identifier: CC0-1.0

//! `UpdateApiKeyCredentialProvider`, and the read that reports what it wrote.
//!
//! The rule under test: an update addresses an EXISTING API key credential
//! provider by `name` — a name this account holds no provider under is the
//! modelled 404, never a provider an update mints — and replaces the secret the
//! provider's key lives in with the one the request describes, by the same
//! `apiKeySecretSource` rule the create follows: a `MANAGED` provider has the
//! request's new key stored in the secret this service manages for it, and an
//! `EXTERNAL` one is pointed at the secret the request references, writing
//! nothing into it. What identifies the provider rather than configures it — the
//! name, the ARN minted from it and the instant it was created — is untouched,
//! `lastUpdatedTime` is the instant of the update, and a later read answers from
//! that one record.
//!
//! A refused update changes nothing: the provider and the secret it named stand
//! exactly as they were.
//!
//! The rows span what the rule generalises over: every transition between the
//! two secret sources (managed to managed, managed to external, external to
//! managed, external to external), the source named and left to the rule, the
//! size of the new key (one character up to the longest a managed secret's
//! document can carry), the provider's name (the shortest and the longest
//! `CredentialProviderName` admits, and the other characters its pattern
//! admits), and the Region the request is signed for. The recorded case is one
//! row.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region the rows are signed for unless one names another.
const REGION: &str = "us-east-1";

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The token vault a request naming none addresses, which the provider's ARN
/// nests it under.
const VAULT: &str = "default";

/// The longest name `CredentialProviderName` admits: length 1..128, pattern
/// `^[a-zA-Z0-9\-_]+$` (`smithy_query(query="operation",
/// operation="UpdateApiKeyCredentialProvider")`).
const MAX_PROVIDER_NAME_LEN: usize = 128;

/// The longest API key `DefaultApiKeyType` admits: length ..65536
/// (`smithy_query(query="operation", operation="UpdateApiKeyCredentialProvider")`),
/// less the characters the `{"apiKey":""}` document a managed secret's value is
/// spends on itself — Secrets Manager holds at most 65536 characters of secret
/// value.
const MAX_API_KEY_LEN: usize = 65_536;
const MANAGED_DOCUMENT_OVERHEAD: usize = 13;

/// The name the secret holding a managed provider's key is created under, and the
/// JSON key that secret's value carries the key under — the layout of a secret
/// the SERVICE manages, which is why no response reports it. Spelled out here
/// rather than read off the implementation, so a drift in either is a failing
/// test.
fn managed_secret_name(provider: &str) -> String {
    format!("bedrock-agentcore-identity/{VAULT}/apikey/{provider}")
}
const MANAGED_SECRET_JSON_KEY: &str = "apiKey";

/// The ARN a provider named `name` carries in `region`: the token-vault path of
/// `ApiKeyCredentialProviderArnType`, in the ARN namespace of the service that
/// holds the vault.
fn provider_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:token-vault/{VAULT}/\
         apikeycredentialprovider/{name}"
    )
}

/// The prefix the ARN of a secret named `name` carries in `region`: a created
/// secret's ARN ends in six characters that belong to the secret rather than to
/// its name.
fn secret_arn_prefix(region: &str, name: &str) -> String {
    format!("arn:aws:secretsmanager:{region}:{ACCOUNT}:secret:{name}-")
}

/// The emulator the rows run against: the control plane, and Secrets Manager
/// beside it on the one shared engine — what the assembled emulator is
/// (`core/router`'s `build_router` registers every service crate), and what the
/// provider's secret ARN has to name a real secret in.
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
        self.control("/identities/CreateApiKeyCredentialProvider", region, body)
            .await
    }

    async fn update_provider(&self, region: &str, body: &Value) -> Answer {
        self.control("/identities/UpdateApiKeyCredentialProvider", region, body)
            .await
    }

    async fn get_provider(&self, region: &str, name: &str) -> Answer {
        self.control(
            "/identities/GetApiKeyCredentialProvider",
            region,
            &json!({"name": name}),
        )
        .await
    }

    /// One Secrets Manager call (`awsJson1_1`, `secretsmanager.<Operation>`)
    /// signed for `region`. The credential scope names `secretsmanager`, which is
    /// what the router claims on.
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

    /// The string value Secrets Manager holds for `secret_id`.
    async fn secret_string(&self, case: &str, region: &str, secret_id: &str) -> String {
        let answer = self
            .secrets(region, "GetSecretValue", &json!({"SecretId": secret_id}))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from GetSecretValue for {secret_id:?}, got {} and \
             body {}",
            answer.status, answer.body
        );
        answer.body["SecretString"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a SecretString for {secret_id:?}, got body {}",
                    answer.body
                )
            })
            .to_string()
    }

    /// Create a secret of the CALLER's own, as an external provider references
    /// one, and report its ARN.
    async fn put_external_secret(
        &self,
        case: &str,
        region: &str,
        name: &str,
        value: &str,
    ) -> String {
        let answer = self
            .secrets(
                region,
                "CreateSecret",
                // A create stakes the secret's initial version, which a raw HTTP
                // request has to name: `ClientRequestToken` is 32..64 characters
                // (`smithy_query(query="operation", operation="CreateSecret",
                // model_path="services/service-com-amazonaws-secretsmanager-\
                // secretsmanager/smithy-model/\
                // com-amazonaws-secretsmanager-secretsmanager.json")`).
                &json!({
                    "Name": name,
                    "SecretString": value,
                    "ClientRequestToken": format!("{:0>32}", name.replace('/', "-")),
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

/// Where a provider keeps its API key, as ONE request describes it.
#[derive(Debug, Clone)]
enum Keeping {
    /// The service manages the secret: `key` is what goes into it. `named` is
    /// whether the request spells `apiKeySecretSource` out or leaves it to the
    /// rule.
    Managed { key: String, named: bool },
    /// The caller manages the secret: it is created before the request is made,
    /// under `secret_name` holding `secret_value`, and the key is extracted with
    /// `json_key`. `named` is whether the request spells the source out or lets
    /// the reference imply it.
    External {
        secret_name: String,
        secret_value: String,
        json_key: String,
        named: bool,
    },
}

impl Keeping {
    fn managed(key: &str, named: bool) -> Self {
        Self::Managed {
            key: key.to_string(),
            named,
        }
    }

    fn external(secret_name: &str, secret_value: &str, json_key: &str, named: bool) -> Self {
        Self::External {
            secret_name: secret_name.to_string(),
            secret_value: secret_value.to_string(),
            json_key: json_key.to_string(),
            named,
        }
    }

    /// The `apiKeySecretSource` a provider described this way ends up with.
    fn effective(&self) -> &'static str {
        match self {
            Self::Managed { .. } => "MANAGED",
            Self::External { .. } => "EXTERNAL",
        }
    }

    /// The members of a create or update request that describe this secret, with
    /// the caller's own secret (if any) put into Secrets Manager first — whose
    /// ARN is reported back so a row can assert the provider reports exactly it.
    async fn request_members(
        &self,
        emulator: &Emulator,
        case: &str,
        region: &str,
        body: &mut Value,
    ) -> Option<String> {
        match self {
            Self::Managed { key, named } => {
                body["apiKey"] = json!(key);
                if *named {
                    body["apiKeySecretSource"] = json!("MANAGED");
                }
                None
            }
            Self::External {
                secret_name,
                secret_value,
                json_key,
                named,
            } => {
                let arn = emulator
                    .put_external_secret(case, region, secret_name, secret_value)
                    .await;
                body["apiKeySecretConfig"] =
                    json!({"secretId": secret_name, "jsonKey": json_key});
                if *named {
                    body["apiKeySecretSource"] = json!("EXTERNAL");
                }
                Some(arn)
            }
        }
    }
}

/// One provider a row creates and then updates.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    name: String,
    region: &'static str,
    /// How the create described the provider's secret.
    before: Keeping,
    /// How the update describes it.
    after: Keeping,
}

/// The updates the rule is asserted over: one row per thing it generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a managed key replaced by another, the source left to the rule then named",
            name: "a".to_string(),
            region: REGION,
            before: Keeping::managed("k", false),
            after: Keeping::managed("sk-live-9f8e7d6c5b4a", true),
        },
        Row {
            case: "a managed provider re-pointed at a secret the caller manages",
            name: "partner-api_key_2026".to_string(),
            region: REGION,
            before: Keeping::managed("first-key", true),
            after: Keeping::external(
                "partner/api/credentials",
                r#"{"apiKey":"caller-owned-key"}"#,
                "apiKey",
                true,
            ),
        },
        Row {
            case: "an external provider taken over by the service, in a non-default Region",
            name: "vendor_handover".to_string(),
            region: "eu-west-2",
            before: Keeping::external(
                "vendor.one@credentials",
                r#"{"token":"caller-owned-token"}"#,
                "token",
                false,
            ),
            after: Keeping::managed(&"x".repeat(4_096), false),
        },
        Row {
            case: "one caller-managed secret swapped for another, the source implied by the \
                   reference alone",
            name: format!("L{}", "o".repeat(MAX_PROVIDER_NAME_LEN - 1)),
            region: "ap-southeast-2",
            before: Keeping::external(
                "first/vendor/secret",
                r#"{"apiKey":"first-caller-owned-key"}"#,
                "apiKey",
                true,
            ),
            after: Keeping::external(
                "second.vendor@secret",
                r#"{"credential":"second-caller-owned-key"}"#,
                "credential",
                false,
            ),
        },
        Row {
            case: "the longest API key a managed secret's document can carry",
            name: "long_key_provider".to_string(),
            region: REGION,
            before: Keeping::managed("short", false),
            after: Keeping::managed(
                &"y".repeat(MAX_API_KEY_LEN - MANAGED_DOCUMENT_OVERHEAD),
                true,
            ),
        },
    ]
}

/// The API key a managed secret's value carries, under the JSON key the service's
/// own document layout holds it at.
fn managed_key(case: &str, stored: &str) -> String {
    let document: Value = serde_json::from_str(stored).unwrap_or_else(|error| {
        panic!("case {case:?}: a managed secret's value is a JSON document, got {error}")
    });
    document[MANAGED_SECRET_JSON_KEY]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the API key under {MANAGED_SECRET_JSON_KEY:?} in the \
                 managed secret"
            )
        })
        .to_string()
}

/// Assert the members describing `keeping`'s secret are the ones `body` reports,
/// and report the secret ARN it named.
fn assert_secret_members(
    case: &str,
    body: &Value,
    keeping: &Keeping,
    region: &str,
    provider: &str,
    external_arn: Option<&str>,
) -> String {
    assert_eq!(
        body["apiKeySecretSource"],
        json!(keeping.effective()),
        "case {case:?}: expected the effective secret source, got body {body}"
    );
    let secret_arn = body["apiKeySecretArn"]["secretArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an apiKeySecretArn, got body {body}"))
        .to_string();
    match (keeping, external_arn) {
        (Keeping::Managed { .. }, _) => {
            let prefix = secret_arn_prefix(region, &managed_secret_name(provider));
            assert!(
                secret_arn.starts_with(&prefix) && secret_arn.len() > prefix.len(),
                "case {case:?}: a managed provider's key lives in the secret derived for it, \
                 expected an ARN under {prefix:?}, got {secret_arn:?}"
            );
            assert_eq!(
                body["apiKeySecretJsonKey"],
                Value::Null,
                "case {case:?}: a provider whose secret the service manages reports no JSON key, \
                 got body {body}"
            );
        }
        (Keeping::External { json_key, .. }, Some(arn)) => {
            assert_eq!(
                secret_arn, arn,
                "case {case:?}: an external provider reports the ARN of the secret it references, \
                 got {secret_arn:?}"
            );
            assert_eq!(
                body["apiKeySecretJsonKey"],
                json!(json_key),
                "case {case:?}: expected the caller's jsonKey, got body {body}"
            );
        }
        (Keeping::External { .. }, None) => {
            panic!("case {case:?}: an external secret must have been created for this request")
        }
    }
    secret_arn
}

/// Assert the secret `keeping` describes really holds what the request asked of
/// it: the new key for a secret the service manages, and the caller's own value
/// untouched for one it does not.
async fn assert_secret_contents(
    emulator: &Emulator,
    case: &str,
    region: &str,
    secret_arn: &str,
    keeping: &Keeping,
) {
    let stored = emulator.secret_string(case, region, secret_arn).await;
    match keeping {
        Keeping::Managed { key, .. } => {
            let held = managed_key(case, &stored);
            assert_eq!(
                held.len(),
                key.len(),
                "case {case:?}: expected the stored key to be {} characters, got {}",
                key.len(),
                held.len()
            );
            assert!(
                held == *key,
                "case {case:?}: expected the managed secret to carry the key the request sent, \
                 got a different value"
            );
        }
        Keeping::External { secret_value, .. } => {
            assert_eq!(
                stored, *secret_value,
                "case {case:?}: an external provider's secret is left exactly as the caller \
                 stored it"
            );
        }
    }
}

/// A served update replaces the provider's secret as the request describes it,
/// leaves what identifies the provider alone, and a later read answers from that
/// one record.
#[tokio::test]
async fn an_updated_provider_reads_back_with_its_new_secret() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();

        let mut create_body = json!({"name": row.name});
        let created_external = row
            .before
            .request_members(&emulator, case, row.region, &mut create_body)
            .await;
        let created = emulator.create_provider(row.region, &create_body).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201 from CreateApiKeyCredentialProvider, got {} and \
             body {}",
            created.status, created.body
        );
        let created_secret_arn = assert_secret_members(
            case,
            &created.body,
            &row.before,
            row.region,
            &row.name,
            created_external.as_deref(),
        );
        let before_read = emulator.get_provider(row.region, &row.name).await;
        let created_time = before_read.instant(case, "createdTime");

        let mut update_body = json!({"name": row.name});
        let updated_external = row
            .after
            .request_members(&emulator, case, row.region, &mut update_body)
            .await;
        let updated = emulator.update_provider(row.region, &update_body).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected HTTP 200 from UpdateApiKeyCredentialProvider, got {} and \
             body {}",
            updated.status, updated.body
        );

        // What identifies the provider is untouched by an update that configures
        // it.
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
            "case {case:?}: an update leaves the instant the provider was created alone, got \
             body {}",
            updated.body
        );
        assert!(
            updated.instant(case, "lastUpdatedTime") >= created_time,
            "case {case:?}: expected lastUpdatedTime at or after createdTime {created_time}, got \
             {}",
            updated.instant(case, "lastUpdatedTime")
        );

        // The secret the update reports is the one the request asked for, and it
        // really holds what the request asked of it.
        let updated_secret_arn = assert_secret_members(
            case,
            &updated.body,
            &row.after,
            row.region,
            &row.name,
            updated_external.as_deref(),
        );
        assert_secret_contents(
            &emulator,
            case,
            row.region,
            &updated_secret_arn,
            &row.after,
        )
        .await;
        if matches!(row.before, Keeping::Managed { .. })
            && matches!(row.after, Keeping::Managed { .. })
        {
            assert_eq!(
                updated_secret_arn, created_secret_arn,
                "case {case:?}: the secret the service manages for one provider is the one \
                 secret, so a replaced key stays in it"
            );
        }

        // The round trip: the read reports the one record the update wrote.
        let read = emulator.get_provider(row.region, &row.name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetApiKeyCredentialProvider, got {} and body \
             {}",
            read.status, read.body
        );
        assert_eq!(
            read.body, updated.body,
            "case {case:?}: the read must report exactly what the update answered"
        );
    }
}

/// The provider an update names has to exist: a name this account holds no
/// provider under is the modelled 404, and the update mints nothing — no
/// provider, and no secret for one.
#[tokio::test]
async fn an_update_of_an_unknown_name_is_not_found() {
    let emulator = Emulator::new();
    let held = "stored_provider";
    let created = emulator
        .create_provider(REGION, &json!({"name": held, "apiKey": "stored-key"}))
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
            .update_provider(REGION, &json!({"name": name, "apiKey": "new-key"}))
            .await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: expected the modelled 404 for {name:?}, got {} and body {}",
            refused.status,
            refused.body
        );
        // The refusal names the resource type and that it is not there; nothing
        // of the request is echoed.
        assert_eq!(
            refused.message(),
            "ApiKey CredentialProvider not found",
            "case {case:?}: unexpected refusal wording for {name:?}"
        );
        let read = emulator.get_provider(REGION, name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused update records no provider, got {} and body {}",
            read.status, read.body
        );
    }

    // Exactly the one secret the create minted stands: a refused update stored
    // nothing for a provider that does not exist.
    let names = emulator.secret_names("a refused update", REGION).await;
    assert_eq!(
        names,
        vec![managed_secret_name(held)],
        "a refused update may mint no secret, got {names:?}"
    );
}

/// The two rules the model states in prose hold for an update as they do for a
/// create — an external provider needs the reference it is external to, and a
/// managed one needs the key the service stores — and a refused update leaves the
/// provider exactly as it was.
#[tokio::test]
async fn an_update_missing_what_its_source_needs_is_refused() {
    let name = "guarded_provider";
    for (case, member, request) in [
        (
            "EXTERNAL with no secret reference",
            "apiKeySecretConfig",
            json!({"name": name, "apiKeySecretSource": "EXTERNAL"}),
        ),
        (
            "MANAGED with no API key at all",
            "apiKey",
            json!({"name": name, "apiKeySecretSource": "MANAGED"}),
        ),
        (
            "the default source with an empty API key",
            "apiKey",
            json!({"name": name, "apiKey": ""}),
        ),
    ] {
        let emulator = Emulator::new();
        let created = emulator
            .create_provider(REGION, &json!({"name": name, "apiKey": "original-key"}))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: the provider to update must be there, got {} and body {}",
            created.status, created.body
        );
        let before = emulator.get_provider(REGION, name).await;

        let refused = emulator.update_provider(REGION, &request).await;
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
        // secret still holds the key the create stored.
        let after = emulator.get_provider(REGION, name).await;
        assert_eq!(
            after.body, before.body,
            "case {case:?}: a refused update may not change the provider"
        );
        let secret_arn = before.body["apiKeySecretArn"]["secretArn"]
            .as_str()
            .expect("a served create reports its secret ARN");
        let stored = emulator.secret_string(case, REGION, secret_arn).await;
        let held = managed_key(case, &stored);
        assert_eq!(
            held, "original-key",
            "case {case:?}: a refused update may not replace the stored key, got {held:?}"
        );
    }
}

/// An update pointing the provider at a secret nobody holds is refused, and the
/// provider keeps the secret it had: the ARN its answer would carry has to name a
/// secret Secrets Manager really holds.
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
            .create_provider(REGION, &json!({"name": name, "apiKey": "original-key"}))
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
                    "apiKeySecretSource": "EXTERNAL",
                    "apiKeySecretConfig": {"secretId": secret_id, "jsonKey": "apiKey"},
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
            "case {case:?}: a provider whose new secret cannot be resolved is left as it was"
        );
    }
}
