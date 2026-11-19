// SPDX-License-Identifier: CC0-1.0

//! `GetResourceApiKey`: the AgentCore Identity exchange of a workload identity token for a
//! resource's API key.
//!
//! RULE: the data plane's first-party resource-credential call CHECKS the presented
//! `workloadIdentityToken` against the issuances the identity plane recorded — that token is a
//! credential the request CARRIES, so a value no live issuance stands behind is
//! `AccessDeniedException` (HTTP 403) with the fixed sentence `Invalid workload access token`
//! and nothing the request
//! named is resolved — then resolves `resourceCredentialProviderName` against the plane that
//! OWNS the account's token vault, refusing a name that plane holds no provider under with
//! `ResourceNotFoundException` (HTTP 404) and the fixed sentence `Resource Credential
//! Provider not found`, and otherwise answers HTTP 200 with the key the provider's secret
//! really holds.
//!
//! The suite lives here rather than in either service's own crate because the rule spans
//! three services over one engine: the token is minted by the AgentCore DATA plane, the
//! credential provider is created by the AgentCore CONTROL plane, and the key itself lives in
//! a Secrets Manager secret — a single-service router has no provider to resolve and no
//! secret to read.
//!
//! The rows span what the rule generalises over: how the provider's secret is sourced (one
//! the service manages, one the caller holds, one held in another Region), the form of the
//! provider name and of the key, which mint the presented token came from (for a user, for
//! the workload itself, two distinct tokens of the same workload and user), why no live
//! issuance stands behind a presented value, and whether the provider still exists when the
//! exchange is made.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as data_plane;

/// The Region every request is signed for unless a row says otherwise. Shared with the
/// sibling suite that drives the same exchange's token gate.
pub(crate) const REGION: &str = "us-east-1";

/// A second Region, for the external secret a provider of this account references from
/// elsewhere.
const OTHER_REGION: &str = "eu-west-1";

/// The workload identity the tokens under test are minted from, and the user they act for.
pub(crate) const WORKLOAD: &str = "simreal.resource-api-key";
pub(crate) const USER: &str = "user-1";

/// `GetResourceApiKey`: `POST /identities/api-key` (source: smithy_query query="operation"
/// operation="GetResourceApiKey": `http`).
const API_KEY_ROUTE: &str = "/identities/api-key";

/// The two mints of the identity plane a token under test can come from (same source).
const TOKEN_FOR_USER_ROUTE: &str = "/identities/GetWorkloadAccessTokenForUserId";
const TOKEN_FOR_WORKLOAD_ROUTE: &str = "/identities/GetWorkloadAccessToken";

/// The control plane's credential-provider routes (source: smithy_query query="operation"
/// operation="CreateApiKeyCredentialProvider,UpdateApiKeyCredentialProvider,\
/// DeleteApiKeyCredentialProvider": `http`).
const CREATE_PROVIDER_ROUTE: &str = "/identities/CreateApiKeyCredentialProvider";
const UPDATE_PROVIDER_ROUTE: &str = "/identities/UpdateApiKeyCredentialProvider";
const DELETE_PROVIDER_ROUTE: &str = "/identities/DeleteApiKeyCredentialProvider";

/// The sentence AWS refuses a credential provider this account does not hold with: fixed
/// text, carrying no part of the request. Spelled out here rather than read off the
/// emulator, so the assertion states the expectation instead of restating the code.
const PROVIDER_NOT_FOUND: &str = "Resource Credential Provider not found";

/// The one sentence the identity plane refuses a presented WORKLOAD IDENTITY token with.
const WORKLOAD_TOKEN_REFUSED: &str = "Invalid workload access token";

/// One answer from the emulator: its status and its JSON body.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Value,
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

    /// The modelled error code this answer carries, with the shape namespace stripped.
    pub(crate) fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    pub(crate) fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    pub(crate) fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The AgentCore control plane, the AgentCore data plane and Secrets Manager over ONE shared
/// engine, exactly as `router/src/lib.rs` mounts them.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(data_plane::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router, engine }
    }

    /// The engine the three services share, so a suite can read the stores the handlers
    /// wrote rather than infer them from a response.
    pub(crate) fn engine(&self) -> &engine::InMemoryEngine {
        &self.engine
    }

    /// One `restJson1` call to the AgentCore signing name, signed for `region`. The ROUTE is
    /// what picks the plane that answers it: the two planes share a signing name.
    async fn agentcore(&self, region: &str, route: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!(
                "https://bedrock-agentcore.{region}.amazonaws.com{route}"
            ))
            .header("host", format!("bedrock-agentcore.{region}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/bedrock-agentcore/\
                     aws4_request, SignedHeaders=host, Signature=abc"
                ),
            )
            .header("content-type", "application/json")
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// One Secrets Manager call (`awsJson1_1`, `secretsmanager.<Operation>`), signed for the
    /// signing name the router claims that service on.
    async fn secrets(&self, region: &str, operation: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://secretsmanager.{region}.amazonaws.com/"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/secretsmanager/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
                ),
            )
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("secretsmanager.{operation}"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        Answer::of(self.router.dispatch(request).await)
    }

    /// Mint a workload access token for a USER, as a workload acting on behalf of one does.
    pub(crate) async fn token_for_user(&self, case: &str) -> String {
        let answer = self
            .agentcore(
                REGION,
                TOKEN_FOR_USER_ROUTE,
                &json!({"workloadName": WORKLOAD, "userId": USER}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the fixture mint must answer 200, got {answer:?}"
        );
        let token = answer.text("workloadAccessToken");
        assert!(
            !token.is_empty(),
            "case {case:?}: the fixture mint must report a token, got {answer:?}"
        );
        token
    }

    /// Mint one for the WORKLOAD itself, which acts for no user.
    async fn token_for_workload(&self, case: &str) -> String {
        let answer = self
            .agentcore(
                REGION,
                TOKEN_FOR_WORKLOAD_ROUTE,
                &json!({"workloadName": WORKLOAD}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the fixture mint must answer 200, got {answer:?}"
        );
        answer.text("workloadAccessToken")
    }

    /// `GetResourceApiKey` — the call under test.
    pub(crate) async fn resource_api_key(&self, token: &str, provider: &str) -> Answer {
        self.agentcore(
            REGION,
            API_KEY_ROUTE,
            &json!({
                "workloadIdentityToken": token,
                "resourceCredentialProviderName": provider,
            }),
        )
        .await
    }

    /// Create an API key credential provider whose key the SERVICE stores for it.
    pub(crate) async fn create_managed_provider(&self, case: &str, name: &str, api_key: &str) {
        let answer = self
            .agentcore(
                REGION,
                CREATE_PROVIDER_ROUTE,
                &json!({"name": name, "apiKey": api_key}),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case {case:?}: the fixture provider {name:?} must be created, got {answer:?}"
        );
    }

    /// Create one that references a secret the CALLER holds, in `region`, and report the
    /// secret's ARN.
    async fn create_external_provider(
        &self,
        case: &str,
        region: &str,
        name: &str,
        json_key: &str,
        api_key: &str,
    ) -> String {
        let secret_name = format!("{name}-held-by-the-caller");
        let created = self
            .secrets(
                region,
                "CreateSecret",
                // A create stakes the secret's initial version, which a raw HTTP request has
                // to name: `ClientRequestToken` is 32..64 characters (smithy_query
                // query="operation" operation="CreateSecret",
                // model_path="services/service-com-amazonaws-secretsmanager-secretsmanager/\
                // smithy-model/com-amazonaws-secretsmanager-secretsmanager.json").
                &json!({
                    "Name": secret_name,
                    "SecretString": json!({json_key: api_key}).to_string(),
                    "ClientRequestToken": format!("{secret_name:0>32}"),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture secret {secret_name:?} must be created, got {created:?}"
        );
        let secret_arn = created.text("ARN");
        assert!(
            secret_arn.contains(&format!(":{region}:")),
            "case {case:?}: the fixture secret must be held in {region}, got {secret_arn:?}"
        );
        let answer = self
            .agentcore(
                region,
                CREATE_PROVIDER_ROUTE,
                &json!({
                    "name": name,
                    "apiKeySecretSource": "EXTERNAL",
                    "apiKeySecretConfig": {"secretId": secret_arn, "jsonKey": json_key},
                }),
            )
            .await;
        assert_eq!(
            answer.status, 201,
            "case {case:?}: the fixture provider {name:?} must be created, got {answer:?}"
        );
        secret_arn
    }

    /// The string value Secrets Manager holds for `secret_id`, read through its own API.
    async fn secret_string(&self, case: &str, region: &str, secret_id: &str) -> String {
        let answer = self
            .secrets(region, "GetSecretValue", &json!({"SecretId": secret_id}))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetSecretValue for {secret_id:?} must answer 200, got {answer:?}"
        );
        answer.text("SecretString")
    }

    /// Record an issuance whose window has already closed, the way a mint records one: a
    /// token minted an hour ago for a fifteen-minute window. The test reaches into the store
    /// the handlers share instead of waiting for a clock, which is not an emulator behaviour.
    pub(crate) fn aged_issuance(&self) -> String {
        let issued_at = SystemTime::now() - Duration::from_secs(3_600);
        let expires_at = issued_at + Duration::from_secs(900);
        let token = data_plane::state::mint_workload_access_token(
            REGION,
            WORKLOAD,
            Some(USER),
            issued_at,
            expires_at,
            7,
        );
        let state =
            data_plane::state::service_state(&*self.engine).expect("the data plane's own store");
        state.lock().workload_access_tokens.record(
            token.clone(),
            data_plane::state::IssuedWorkloadAccessToken {
                workload_name: WORKLOAD.to_string(),
                user_id: Some(USER.to_string()),
                issued_at,
                expires_at,
            },
            issued_at,
        );
        token
    }
}

/// How many issuances the identity plane holds — what a test checks to show a refused
/// exchange recorded nothing of its own.
#[must_use]
pub(crate) fn issuance_count(engine: &engine::InMemoryEngine) -> usize {
    let state = data_plane::state::service_state(engine).expect("the data plane's own store");
    let store = state.lock();
    store.workload_access_tokens.len()
}

/// A token of this plane's own FORM that this emulator never issued, minted for `region` and
/// for `user` (`None` for the workload itself) with the mint sequence `seq`, and recorded
/// nowhere.
#[must_use]
pub(crate) fn token_minted_nowhere(region: &str, user: Option<&str>, seq: u64) -> String {
    let issued_at = SystemTime::now();
    data_plane::state::mint_workload_access_token(
        region,
        WORKLOAD,
        user,
        issued_at,
        issued_at + Duration::from_secs(900),
        seq,
    )
}

/// A token of the right FORM that this emulator never issued: the identity plane's own mint
/// of one, built here and recorded nowhere.
fn unissued_token() -> String {
    let issued_at = SystemTime::now();
    data_plane::state::mint_workload_access_token(
        REGION,
        WORKLOAD,
        Some(USER),
        issued_at,
        issued_at + Duration::from_secs(900),
        4_242,
    )
}

/// How the provider under test keeps its key, which is what each row of the success table
/// varies.
#[derive(Debug, Clone)]
enum Provider {
    /// The service manages the secret the key is stored in.
    Managed,
    /// The caller holds the secret, in `region`, under `json_key`.
    External {
        region: &'static str,
        json_key: &'static str,
    },
}

/// A freshly minted workload identity token exchanges for exactly the key the named provider
/// holds, whichever way that provider's secret is sourced.
#[tokio::test]
async fn a_minted_token_exchanges_for_the_key_the_named_provider_holds() {
    let emulator = Emulator::new();

    // Rows: how the secret is sourced × the form of the provider name × the form of the key.
    let rows: Vec<(&str, &str, String, Provider)> = vec![
        (
            "a service-managed secret",
            "partner-api",
            "sk-live-0001".to_string(),
            Provider::Managed,
        ),
        (
            "a name of digits and both punctuation characters the name admits",
            "partner_api-2024",
            "sk-live-0002".to_string(),
            Provider::Managed,
        ),
        (
            "the shortest name and key the shapes admit",
            "p",
            "k".to_string(),
            Provider::Managed,
        ),
        (
            "a key of six hundred characters",
            "long-key-provider",
            "k".repeat(600),
            Provider::Managed,
        ),
        (
            "a key whose characters are not alphanumeric",
            "punctuated-key-provider",
            "sk/live+0003=AbC.dEf!".to_string(),
            Provider::Managed,
        ),
        (
            "a secret the caller holds",
            "partner-external",
            "pk-0004".to_string(),
            Provider::External {
                region: REGION,
                json_key: "partnerKey",
            },
        ),
        (
            "a secret the caller holds in another Region",
            "partner-elsewhere",
            "pk-0005".to_string(),
            Provider::External {
                region: OTHER_REGION,
                json_key: "apiKeyValue",
            },
        ),
    ];

    // Every row's provider is created first, so each exchange below runs against the whole
    // set rather than against one provider at a time.
    let mut secrets: Vec<(&str, Option<(&str, String)>)> = Vec::new();
    for (case, name, api_key, provider) in &rows {
        match provider {
            Provider::Managed => {
                emulator.create_managed_provider(case, name, api_key).await;
                secrets.push((case, None));
            }
            Provider::External { region, json_key } => {
                let secret_arn = emulator
                    .create_external_provider(case, region, name, json_key, api_key)
                    .await;
                secrets.push((case, Some((region, secret_arn))));
            }
        }
    }

    // Two tokens of the SAME workload and user, as a client that mints twice holds, plus one
    // minted for the workload itself: each is independently usable the moment it is minted.
    let first = emulator.token_for_user("the first mint").await;
    let second = emulator.token_for_user("the second mint").await;
    assert_ne!(
        first, second,
        "two mints must hand out two distinct tokens, got the same value twice"
    );
    let for_the_workload = emulator.token_for_workload("the workload's own mint").await;

    for (token_case, token) in [
        ("a token minted for the user", &first),
        ("a second token minted for the same user", &second),
        ("a token minted for the workload itself", &for_the_workload),
    ] {
        for (case, name, api_key, _) in &rows {
            let answer = emulator.resource_api_key(token, name).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?} with {token_case:?}: expected HTTP 200, got {answer:?}"
            );
            assert_eq!(
                answer.text("apiKey"),
                *api_key,
                "case {case:?} with {token_case:?}: expected the key the provider holds, got {:?}",
                answer.text("apiKey")
            );
        }
    }

    // The key answered is the one Secrets Manager really holds, read back through that
    // service's own API: nothing here is recomputed from the request.
    for ((case, held), (_, _, api_key, _)) in secrets.iter().zip(rows.iter()) {
        if let Some((region, secret_arn)) = held {
            let document = emulator.secret_string(case, region, secret_arn).await;
            assert!(
                document.contains(api_key.as_str()),
                "case {case:?}: the referenced secret must hold the key the exchange answered, \
                 got {document:?}"
            );
        }
    }
}

/// A name the account holds no credential provider under is the modelled 404 with AWS's fixed
/// sentence — which is what proves the presented token was accepted.
#[tokio::test]
async fn a_name_no_provider_exists_under_is_refused_with_the_fixed_sentence() {
    let emulator = Emulator::new();
    emulator
        .create_managed_provider("the one provider that exists", "partner-api", "sk-live-0001")
        .await;
    emulator
        .create_managed_provider("a provider that is then deleted", "retired-api", "sk-gone")
        .await;
    let deleted = emulator
        .agentcore(
            REGION,
            DELETE_PROVIDER_ROUTE,
            &json!({"name": "retired-api"}),
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "the fixture delete must answer 204, got {deleted:?}"
    );

    let first = emulator.token_for_user("the first mint").await;
    let second = emulator.token_for_user("the second mint").await;

    // Rows: a name nothing ever created, one differing from an existing provider only in
    // case, one that is a prefix of it, and one whose provider the owning plane has deleted.
    let absent = [
        ("a name nothing created", "sim2real-nonexistent-provider"),
        ("a name differing only in case", "Partner-API"),
        ("a name that is a prefix of one that exists", "partner"),
        ("a provider the owning plane deleted", "retired-api"),
    ];

    for (token_case, token) in [("the first token", &first), ("the second token", &second)] {
        for (case, name) in absent {
            let answer = emulator.resource_api_key(token, name).await;
            assert_eq!(
                answer.status, 404,
                "case {case:?} with {token_case:?}: expected HTTP 404, got {answer:?}"
            );
            assert_eq!(
                answer.error_code(),
                "ResourceNotFoundException",
                "case {case:?} with {token_case:?}: expected ResourceNotFoundException, got \
                 {answer:?}"
            );
            assert_eq!(
                answer.message(),
                PROVIDER_NOT_FOUND,
                "case {case:?} with {token_case:?}: expected the fixed sentence, got {:?}",
                answer.message()
            );
            // Fixed text: the refusal quotes neither the name asked for nor the token.
            assert!(
                !answer.message().contains(name) && !answer.message().contains(token.as_str()),
                "case {case:?} with {token_case:?}: the sentence must carry no part of the \
                 request, got {:?}",
                answer.message()
            );
        }
    }

    // And the provider that DOES exist still answers its key, so the refusals above are about
    // the name and not about the operation.
    let answer = emulator.resource_api_key(&first, "partner-api").await;
    assert_eq!(
        answer.status, 200,
        "the provider that exists must still answer its key, got {answer:?}"
    );
    assert_eq!(
        answer.text("apiKey"),
        "sk-live-0001",
        "the provider that exists must answer the key it holds, got {:?}",
        answer.text("apiKey")
    );
}

/// A presented value no live issuance of this service stands behind is the modelled 403 —
/// decided before the provider the request named is looked up at all. The workload identity
/// token is a credential carried in the request body, so refusing it is an authorization
/// verdict; the modelled `UnauthorizedException` (401) belongs to the caller's own OAuth
/// bearer credential, which this call presents none of.
#[tokio::test]
async fn a_token_no_live_issuance_stands_behind_is_refused_access_denied() {
    let emulator = Emulator::new();
    emulator
        .create_managed_provider("the provider that exists", "partner-api", "sk-live-0001")
        .await;
    let aged = emulator.aged_issuance();
    let unissued = unissued_token();

    // Rows: why no live issuance stands behind the value × whether the provider named exists.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "an issuance whose window has closed",
            aged.clone(),
            "partner-api",
        ),
        (
            "an issuance whose window has closed, naming no provider that exists",
            aged,
            "sim2real-nonexistent-provider",
        ),
        (
            "a well-formed token this service never issued",
            unissued.clone(),
            "partner-api",
        ),
        (
            "the same token naming no provider that exists",
            unissued.clone(),
            "sim2real-nonexistent-provider",
        ),
        (
            "the same token presented under the bearer scheme",
            format!("Bearer {unissued}"),
            "partner-api",
        ),
        (
            "three segments that are not base64url JSON",
            "abc.def.ghi".to_string(),
            "partner-api",
        ),
        (
            "a value that is no token at all",
            "not-a-token".to_string(),
            "partner-api",
        ),
    ];

    for (case, token, provider) in rows {
        let answer = emulator.resource_api_key(&token, provider).await;
        assert_eq!(
            answer.status, 403,
            "case {case:?}: expected HTTP 403, got {answer:?}"
        );
        assert_eq!(
            answer.error_code(),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            WORKLOAD_TOKEN_REFUSED,
            "case {case:?}: expected {WORKLOAD_TOKEN_REFUSED:?}, got {:?}",
            answer.message()
        );
        // The refusal never echoes the bearer value back.
        assert!(
            !answer.message().contains(&token),
            "case {case:?}: the refusal must not quote the presented token, got {:?}",
            answer.message()
        );
    }

    // A live token presented against the same existing provider is served, so the refusals
    // above are about the token and not about the provider.
    let live = emulator.token_for_user("a live mint").await;
    let answer = emulator.resource_api_key(&live, "partner-api").await;
    assert_eq!(
        answer.status, 200,
        "a live token must be served, got {answer:?}"
    );
}

/// The key the exchange answers tracks the provider the OWNING plane holds at this instant:
/// an update that replaces the key is answered by the next exchange, and a delete ends it.
#[tokio::test]
async fn the_key_answered_tracks_the_provider_the_owning_plane_holds() {
    let emulator = Emulator::new();
    emulator
        .create_managed_provider("the provider under test", "rotating-api", "sk-live-first")
        .await;
    let token = emulator.token_for_user("the mint").await;

    let answer = emulator.resource_api_key(&token, "rotating-api").await;
    assert_eq!(
        answer.text("apiKey"),
        "sk-live-first",
        "case \"as created\": expected the key the create stored, got {answer:?}"
    );

    let updated = emulator
        .agentcore(
            REGION,
            UPDATE_PROVIDER_ROUTE,
            &json!({"name": "rotating-api", "apiKey": "sk-live-second"}),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "the fixture update must answer 200, got {updated:?}"
    );
    let answer = emulator.resource_api_key(&token, "rotating-api").await;
    assert_eq!(
        answer.status, 200,
        "case \"after the update\": expected HTTP 200, got {answer:?}"
    );
    assert_eq!(
        answer.text("apiKey"),
        "sk-live-second",
        "case \"after the update\": expected the replacing key, got {answer:?}"
    );

    let deleted = emulator
        .agentcore(
            REGION,
            DELETE_PROVIDER_ROUTE,
            &json!({"name": "rotating-api"}),
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "the fixture delete must answer 204, got {deleted:?}"
    );
    let answer = emulator.resource_api_key(&token, "rotating-api").await;
    assert_eq!(
        answer.status, 404,
        "case \"after the delete\": expected HTTP 404, got {answer:?}"
    );
    assert_eq!(
        answer.message(),
        PROVIDER_NOT_FOUND,
        "case \"after the delete\": expected the fixed sentence, got {:?}",
        answer.message()
    );
}
