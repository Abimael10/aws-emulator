// SPDX-License-Identifier: CC0-1.0

//! `DeleteApiKeyCredentialProvider` end to end over HTTP, with the reads that observe what
//! it did.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against one shared engine —
//! with the emulator's own Secrets Manager mounted beside the control plane, as the
//! assembled emulator mounts it, because that is where the API key itself lives.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="DeleteApiKeyCredentialProvider,\
//! GetApiKeyCredentialProvider")`): the delete is satisfied by the provider's ABSENCE. The
//! model declares it idempotent with an empty 204 success, so for ANY name the request
//! carries it answers 204 with no member and leaves this account holding no provider under
//! that name — whether one was there or not, and however often the call is repeated. The
//! provider carries no status, so the removal is immediate: every later read of that `name` is
//! the modelled `ResourceNotFoundException` (HTTP 404) whose message names the name the
//! request carried, and the name is free for a new provider. The delete reaches only the ONE
//! provider it names — the account's other providers, and the Secrets Manager secret behind
//! each provider's key, are left exactly as they were.
//!
//! The rows differ along what the rule generalises over: whether the account holds a provider
//! under the name at all, where the key lives (`MANAGED`, named and implied, and `EXTERNAL`),
//! the shape and length of the provider's name, the size of the key, and whether the request
//! carried tags. Idempotence is an invariant rather than a value, so it is also driven as a
//! property over generated names and repeat counts, from a reported seed.

use std::sync::Arc;

use bytes::Bytes;
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcore;

/// The Region the rows are signed for.
const REGION: &str = "us-east-1";

/// The token vault a request naming none addresses, which a managed secret's name carries.
const VAULT: &str = "default";

/// The longest name `CredentialProviderName` admits: length 1..128, pattern
/// `^[a-zA-Z0-9\-_]+$` (`smithy_query(query="shape",shape="CredentialProviderName")`).
const MAX_PROVIDER_NAME_LEN: usize = 128;

/// Every character `CredentialProviderName`'s pattern `^[a-zA-Z0-9\-_]+$` admits
/// (`smithy_query(query="shape",shape="CredentialProviderName")`) — the alphabet the
/// idempotence property generates its names from.
const PROVIDER_NAME_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";

/// How many generated names the idempotence property drives: bounded, so the suite stays
/// fast whatever the host.
const IDEMPOTENCE_CASES: usize = 24;

/// The most times one generated name's delete is repeated.
const MAX_DELETE_REPEATS: u32 = 4;

/// A name `CredentialProviderName` admits: 1..=128 characters of its pattern's alphabet.
fn generated_provider_name(rng: &mut StdRng) -> String {
    let length = rng.random_range(1..=MAX_PROVIDER_NAME_LEN);
    (0..length)
        .map(|_| {
            let index = rng.random_range(0..PROVIDER_NAME_ALPHABET.len());
            char::from(PROVIDER_NAME_ALPHABET[index])
        })
        .collect()
}

/// The name the secret holding a managed provider's key is created under. Spelled out here
/// rather than read off the implementation, so a drift in either is a failing test.
fn managed_secret_name(provider: &str) -> String {
    format!("bedrock-agentcore-identity/{VAULT}/apikey/{provider}")
}

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
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// One `restJson1` request to the control plane, signed for this suite's Region — which
    /// is what the router routes on and what the handlers read the Region from.
    async fn control(&self, uri: &str, body: &Value) -> Answer {
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
        Answer::of(self.router.dispatch(request).await)
    }

    async fn create_provider(&self, body: &Value) -> Answer {
        self.control("/identities/CreateApiKeyCredentialProvider", body)
            .await
    }

    async fn get_provider(&self, name: &str) -> Answer {
        self.control(
            "/identities/GetApiKeyCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    async fn delete_provider(&self, name: &str) -> Answer {
        self.control(
            "/identities/DeleteApiKeyCredentialProvider",
            &json!({"name": name}),
        )
        .await
    }

    /// One Secrets Manager call (`awsJson1_1`, `secretsmanager.<Operation>`). The credential
    /// scope names `secretsmanager`, which is what the router claims on.
    async fn secrets(&self, operation: &str, body: &Value) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/secretsmanager/\
             aws4_request, SignedHeaders=host, Signature=abc"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{REGION}.amazonaws.com/"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("secretsmanager.{operation}"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// Create a secret of the CALLER's own, as an external provider references one, and
    /// report its ARN.
    async fn put_external_secret(&self, case: &str, name: &str, value: &str) -> String {
        let answer = self
            .secrets(
                "CreateSecret",
                // A create stakes the secret's initial version, which a raw HTTP request has
                // to name: `ClientRequestToken` is 32..64 characters.
                &json!({
                    "Name": name,
                    "SecretString": value,
                    "ClientRequestToken": format!("{:0>32}", name.replace('/', "-")),
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200 from CreateSecret for {name:?}, got {} ({})",
            answer.status, answer.body
        );
        answer.body["ARN"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a created secret reports its ARN, got {}",
                    answer.body
                )
            })
            .to_string()
    }

    /// What `GetSecretValue` answers for `secret_id`.
    async fn get_secret_value(&self, secret_id: &str) -> Answer {
        self.secrets("GetSecretValue", &json!({"SecretId": secret_id}))
            .await
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
}

/// The sentence the READ route refuses with when this account holds no provider under the
/// name the request addressed. The fixed text is the service's; the only variable part is
/// the name the request carried.
fn not_found_message(name: &str) -> String {
    format!("ApiKeyCredentialProvider not found for {name}")
}

/// Assert one answer is the delete's empty 204 success: the modelled output shape has no
/// members, so the body carries none.
fn assert_empty_success(case: &str, answer: &Answer) {
    let carries_no_member = match &answer.body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    };
    assert!(
        answer.status == 204 && carries_no_member,
        "case {case:?}: the delete answers 204 carrying no member, got {} ({})",
        answer.status,
        answer.body
    );
}

/// Where a row's provider keeps its API key.
enum Source {
    /// The service manages the secret the row's `api_key` goes into. `named` is whether the
    /// request spells `apiKeySecretSource` out or leaves it to the rule.
    Managed { named: bool },
    /// The caller manages the secret: it is created before the provider is, under
    /// `secret_name`, holding `secret_value`, and the key is extracted with `json_key`.
    External {
        secret_name: &'static str,
        secret_value: &'static str,
        json_key: &'static str,
    },
}

/// One row of the deletion table.
struct Row {
    case: &'static str,
    /// The provider's name — the identifier every provider route addresses it by.
    name: String,
    api_key: String,
    source: Source,
    tags: Option<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a managed provider, source left to the rule",
            name: "orders-api-key".to_string(),
            api_key: "sk-orders-0001".to_string(),
            source: Source::Managed { named: false },
            tags: None,
        },
        Row {
            case: "a managed provider with MANAGED named, the shortest name and a \
                   one-character key, with tags",
            name: "a".to_string(),
            api_key: "k".to_string(),
            source: Source::Managed { named: true },
            tags: Some(json!({"team": "payments"})),
        },
        Row {
            case: "a managed provider under the longest name the pattern admits",
            name: format!("{:_>width$}", "Z", width = MAX_PROVIDER_NAME_LEN),
            api_key: "sk-".to_string() + &"9".repeat(512),
            source: Source::Managed { named: false },
            tags: None,
        },
        Row {
            case: "an external provider, whose secret is the caller's own",
            name: "search_API-key_2".to_string(),
            api_key: String::new(),
            source: Source::External {
                secret_name: "my/own/search-key",
                secret_value: r#"{"theKey":"sk-search-0002"}"#,
                json_key: "theKey",
            },
            tags: Some(json!({"owner": "search"})),
        },
    ]
}

impl Row {
    /// The `CreateApiKeyCredentialProvider` body this row sends, once any secret of the
    /// caller's own has been created.
    async fn create_body(&self, emulator: &Emulator) -> Value {
        let mut body = json!({"name": self.name});
        match &self.source {
            Source::Managed { named } => {
                body["apiKey"] = json!(self.api_key);
                if *named {
                    body["apiKeySecretSource"] = json!("MANAGED");
                }
            }
            Source::External {
                secret_name,
                secret_value,
                json_key,
            } => {
                emulator
                    .put_external_secret(self.case, secret_name, secret_value)
                    .await;
                body["apiKeySecretSource"] = json!("EXTERNAL");
                body["apiKeySecretConfig"] =
                    json!({"secretId": secret_name, "jsonKey": json_key});
            }
        }
        if let Some(tags) = &self.tags {
            body["tags"] = tags.clone();
        }
        body
    }
}

/// A deleted provider stops resolving at once: the delete answers 204 carrying no member,
/// every later read of that name is the modelled 404, a repeat of the delete is that same
/// empty 204 success, and the name is free for a new provider.
#[tokio::test]
async fn a_deleted_api_key_credential_provider_stops_resolving() {
    for row in rows() {
        let emulator = Emulator::new();
        let body = row.create_body(&emulator).await;
        let created = emulator.create_provider(&body).await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected HTTP 201 from CreateApiKeyCredentialProvider, got {} ({})",
            row.case, created.status, created.body
        );
        let readable = emulator.get_provider(&row.name).await;
        assert_eq!(
            readable.status, 200,
            "case {:?}: the created provider is readable before the delete, got {} ({})",
            row.case, readable.status, readable.body
        );

        let deleted = emulator.delete_provider(&row.name).await;
        assert_empty_success(row.case, &deleted);

        let gone = emulator.get_provider(&row.name).await;
        assert_eq!(
            (gone.status, gone.error_code(), gone.message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message(&row.name)
            ),
            "case {:?}: a deleted provider is the modelled 404, got {} ({})",
            row.case,
            gone.status,
            gone.body
        );
        // And it stays gone: a read a moment later answers the same way, rather than the
        // resource coming back.
        let still_gone = emulator.get_provider(&row.name).await;
        assert_eq!(
            (still_gone.status, still_gone.message()),
            (404, not_found_message(&row.name)),
            "case {:?}: a retired provider does not come back, got {} ({})",
            row.case,
            still_gone.status,
            still_gone.body
        );

        // The delete is satisfied by absence, so repeating it on the retired name is the
        // same empty success rather than a refusal.
        let repeated = emulator.delete_provider(&row.name).await;
        assert_empty_success(row.case, &repeated);
        let still_gone_after_repeat = emulator.get_provider(&row.name).await;
        assert_eq!(
            (
                still_gone_after_repeat.status,
                still_gone_after_repeat.message()
            ),
            (404, not_found_message(&row.name)),
            "case {:?}: a repeated delete does not resurrect the provider, got {} ({})",
            row.case,
            still_gone_after_repeat.status,
            still_gone_after_repeat.body
        );

        // "The name must be unique within your account" held the name while the provider
        // existed; a deleted provider holds it no longer.
        let recreated = emulator.create_provider(&body).await;
        assert_eq!(
            recreated.status, 201,
            "case {:?}: the name a deleted provider held is free again, got {} ({})",
            row.case, recreated.status, recreated.body
        );
        let read_back = emulator.get_provider(&row.name).await;
        assert_eq!(
            read_back.status, 200,
            "case {:?}: the re-created provider is readable, got {} ({})",
            row.case, read_back.status, read_back.body
        );
    }
}

/// A delete retires the ONE provider it names: the account's other providers stay readable,
/// and the secret a provider's key lives in — the caller's own for an `EXTERNAL` provider,
/// the one the service manages for a `MANAGED` one — is left exactly as it was, so a later
/// create of a provider under the retired name reports the key its own create stored.
#[tokio::test]
async fn deleting_a_provider_leaves_the_other_providers_and_their_secrets_alone() {
    let emulator = Emulator::new();

    // A managed provider: the create put the key into a secret of the service's own.
    let managed = emulator
        .create_provider(&json!({"name": "managed-key", "apiKey": "sk-managed-0003"}))
        .await;
    assert_eq!(
        managed.status, 201,
        "setup: the managed provider must be created, got {} ({})",
        managed.status, managed.body
    );
    let managed_secret = managed_secret_name("managed-key");
    let before = emulator.get_secret_value(&managed_secret).await;
    assert_eq!(
        before.status, 200,
        "setup: the managed secret holds the key before the delete, got {} ({})",
        before.status, before.body
    );

    // An external provider beside it: its secret is the caller's own.
    let external_secret = "my/own/retained-key";
    emulator
        .put_external_secret(
            "the caller's own secret",
            external_secret,
            r#"{"apiKey":"sk-external-0004"}"#,
        )
        .await;
    let external = emulator
        .create_provider(&json!({
            "name": "external-key",
            "apiKeySecretSource": "EXTERNAL",
            "apiKeySecretConfig": {"secretId": external_secret, "jsonKey": "apiKey"},
        }))
        .await;
    assert_eq!(
        external.status, 201,
        "setup: the external provider must be created, got {} ({})",
        external.status, external.body
    );

    let deleted = emulator.delete_provider("managed-key").await;
    assert_eq!(
        deleted.status, 204,
        "the managed provider's delete is served, got {} ({})",
        deleted.status, deleted.body
    );
    let gone = emulator.get_provider("managed-key").await;
    assert_eq!(
        (gone.status, gone.message()),
        (404, not_found_message("managed-key")),
        "the deleted provider resolves nothing, got {} ({})",
        gone.status,
        gone.body
    );

    // The other provider is untouched by that delete, and so is the secret it references.
    let other = emulator.get_provider("external-key").await;
    assert_eq!(
        other.status, 200,
        "deleting one provider leaves the account's other provider readable, got {} ({})",
        other.status, other.body
    );
    let caller_secret = emulator.get_secret_value(external_secret).await;
    assert_eq!(
        caller_secret.status, 200,
        "the secret an external provider references is the caller's own and is left alone, \
         got {} ({})",
        caller_secret.status, caller_secret.body
    );

    // A provider created again under the retired name reports the key ITS create stored,
    // which is what makes the name reusable rather than haunted by the old provider's key.
    let recreated = emulator
        .create_provider(&json!({"name": "managed-key", "apiKey": "sk-managed-0006"}))
        .await;
    assert_eq!(
        recreated.status, 201,
        "the retired name takes a new provider, got {} ({})",
        recreated.status, recreated.body
    );
    let value = emulator.get_secret_value(&managed_secret).await;
    assert_eq!(
        value.status, 200,
        "the new provider's key is readable from the secret the service manages, got {} ({})",
        value.status, value.body
    );
    assert_eq!(
        value.body["SecretString"].as_str().unwrap_or_default(),
        r#"{"apiKey":"sk-managed-0006"}"#,
        "the secret behind the re-created provider carries ITS key, got {}",
        value.body
    );

    let external_deleted = emulator.delete_provider("external-key").await;
    assert_eq!(
        external_deleted.status, 204,
        "the external provider's delete is served, got {} ({})",
        external_deleted.status, external_deleted.body
    );
    let retained = emulator.get_secret_value(external_secret).await;
    assert_eq!(
        retained.status, 200,
        "deleting an external provider does not delete the caller's secret, got {} ({})",
        retained.status, retained.body
    );
}

/// A delete naming a provider this account does not hold is the delete's own empty 204
/// success — repeatable, and deleting nothing. The name it could not resolve is refused only
/// on the READ route.
#[tokio::test]
async fn a_provider_delete_that_resolves_nothing_succeeds_and_deletes_nothing() {
    let emulator = Emulator::new();
    let created = emulator
        .create_provider(&json!({"name": "bystander-key", "apiKey": "sk-bystander-0005"}))
        .await;
    assert_eq!(
        created.status, 201,
        "setup: the provider must be created, got {} ({})",
        created.status, created.body
    );

    // Every name below satisfies `CredentialProviderName` (`^[a-zA-Z0-9\-_]+$`, 1..128), so
    // what the row exercises is the handler's own lookup and not the boundary constraint
    // walk: the shortest name the pattern admits, one carrying both characters its pattern
    // allows beside letters and digits, one at the top of the length range, and one that
    // differs from the stored provider's name only in case.
    let unknown = [
        "x".to_string(),
        "no_such-provider".to_string(),
        format!("{:_>width$}", "Q", width = MAX_PROVIDER_NAME_LEN),
        "Bystander-Key".to_string(),
    ];
    for name in unknown {
        // Twice: the delete is satisfied by the provider's absence, so it answers the same
        // empty success however often it is repeated on a name nothing resolves.
        for attempt in 1..=2 {
            let answered = emulator.delete_provider(&name).await;
            assert_empty_success(&format!("{name:?}, attempt {attempt}"), &answered);
        }
        // The name the delete accepted still resolves nothing on the READ route, which is
        // where the modelled refusal lives.
        let read = emulator.get_provider(&name).await;
        assert_eq!(
            (read.status, read.error_code(), read.message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                not_found_message(&name)
            ),
            "case {name:?}: the read names the provider the request carried, got {} ({})",
            read.status,
            read.body
        );
    }

    let intact = emulator.get_provider("bystander-key").await;
    assert_eq!(
        intact.status, 200,
        "a delete of a name nothing resolves leaves the account's provider readable, got {} \
         ({})",
        intact.status, intact.body
    );
    let secret = emulator
        .get_secret_value(&managed_secret_name("bystander-key"))
        .await;
    assert_eq!(
        secret.status, 200,
        "a delete of a name nothing resolves leaves the secret behind the provider alone, \
         got {} ({})",
        secret.status, secret.body
    );
}

/// The invariant behind the rule, over GENERATED names rather than a hand-picked set: for
/// any name `CredentialProviderName` admits, and whether or not this account holds a
/// provider under it, every delete of that name answers the empty 204 success and leaves
/// the name resolving nothing — so repeating a delete any number of times answers exactly
/// as the first one did. The names are drawn from the pattern's own alphabet across its
/// whole length range; the seed is reported so a failing draw can be replayed.
#[tokio::test]
async fn deleting_any_name_the_model_admits_is_idempotent() {
    /// The provider none of the generated deletes names, so each case also shows the
    /// delete reaches only the name it carries.
    const BYSTANDER: &str = "property-bystander-key";

    let seed: u64 = rand::random();
    println!("deleting_any_name_the_model_admits_is_idempotent: seed {seed}");
    let mut rng = StdRng::seed_from_u64(seed);

    let emulator = Emulator::new();
    let bystander = emulator
        .create_provider(&json!({"name": BYSTANDER, "apiKey": "sk-property-0007"}))
        .await;
    assert_eq!(
        bystander.status, 201,
        "seed {seed}: the bystander provider must be created, got {} ({})",
        bystander.status, bystander.body
    );

    for index in 0..IDEMPOTENCE_CASES {
        let name = generated_provider_name(&mut rng);
        if name == BYSTANDER {
            continue;
        }
        // Half the draws delete a provider this account really holds, half a name it
        // never held: the rule spans both, because the delete is satisfied by absence.
        let held = rng.random_bool(0.5);
        if held {
            let created = emulator
                .create_provider(&json!({"name": name, "apiKey": format!("sk-{index}")}))
                .await;
            assert_eq!(
                created.status, 201,
                "seed {seed}, case {index}, name {name:?}: the provider under test must be \
                 created, got {} ({})",
                created.status, created.body
            );
        }

        let repeats = rng.random_range(1..=MAX_DELETE_REPEATS);
        for attempt in 1..=repeats {
            let answered = emulator.delete_provider(&name).await;
            assert_empty_success(
                &format!(
                    "seed {seed}, case {index}, name {name:?} (held: {held}), attempt \
                     {attempt} of {repeats}"
                ),
                &answered,
            );
            // After every one of them the name resolves nothing, which is what makes the
            // next delete answer the same way.
            let read = emulator.get_provider(&name).await;
            assert_eq!(
                (read.status, read.error_code(), read.message()),
                (
                    404,
                    "ResourceNotFoundException".to_string(),
                    not_found_message(&name)
                ),
                "seed {seed}, case {index}, name {name:?} (held: {held}), attempt {attempt}: \
                 the deleted name resolves nothing, got {} ({})",
                read.status,
                read.body
            );
        }
    }

    let intact = emulator.get_provider(BYSTANDER).await;
    assert_eq!(
        intact.status, 200,
        "seed {seed}: no generated delete reached the bystander provider, got {} ({})",
        intact.status, intact.body
    );
    let secret = emulator.get_secret_value(&managed_secret_name(BYSTANDER)).await;
    assert_eq!(
        secret.status, 200,
        "seed {seed}: the secret behind the bystander provider is left alone, got {} ({})",
        secret.status, secret.body
    );
}
