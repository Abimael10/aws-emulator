// SPDX-License-Identifier: CC0-1.0

//! What an `@idempotencyToken` of this service is BOUND to, driven over HTTP through the
//! real router so every assertion goes through routing, deserialization, the handler and
//! serialization.
//!
//! The rule under test: a token member carrying `@idempotencyToken` is bound, for the
//! operation that first used it, to the whole set of parameters that call presented, so a
//! later call of that operation presenting the same token answers the first call's result
//! and creates nothing when the parameters match, and is refused with the operation's
//! modelled conflict error when any of them differs. `CreateRegistry.clientToken` and
//! `CreateRegistryRecord.clientToken` are the only members of this model carrying the trait
//! (`smithy_query(query="operation", operation="CreateRegistry,CreateRegistryRecord")`),
//! and the model documents the token as a "case-sensitive identifier".
//!
//! `tests/all/idempotency_tokens.rs` pins the rule over the members a request names
//! directly (the required name, an optional scalar, a tag map entry, a member added or
//! dropped). This suite pins the dimensions it leaves open: the nested configuration
//! structures a create request carries, the case of the token itself, the operation the
//! token is scoped to, and whether the binding outlives the asynchronous creation the clock
//! owes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region in the test's SigV4 credential scope.
const REGION: &str = "us-west-2";

/// A signed-looking request, so the router resolves this service's signing name
/// (`agent-registry`). The signature itself is never checked: the test router runs with
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// How long a wait for a transition the clock owes may take before the test gives up.
/// Generous on purpose: the test waits for the CONDITION, so a loaded or slow host only
/// polls more times.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long the poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// What a create answers when a token is presented with other parameters. Wholly fixed
/// text: it names neither the token nor any parameter.
const PARAMETERS_DIFFER: &str = "Idempotency token already used with different request parameters";

/// A customer-managed key ARN of `KmsKeyArn`'s shape
/// (`smithy_query(query="shape", shape="EncryptionConfiguration")`).
const KMS_KEY_ONE: &str = "arn:aws:kms:us-west-2:123456789012:key/11111111-2222-3333-4444-555555555555";

/// A second key ARN of the same shape, so a row can differ in the key and nothing else.
const KMS_KEY_TWO: &str = "arn:aws:kms:us-west-2:123456789012:key/66666666-7777-8888-9999-000000000000";

/// One HTTP answer, reduced to what these tests assert on.
struct Reply {
    status: u16,
    body: Value,
    /// The wire error code (`x-amzn-errortype`), present only on a failure.
    error_code: Option<String>,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    fn message(&self) -> String {
        self.body["message"]
            .as_str()
            .or_else(|| self.body["Message"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService::new(
            engine,
        ),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://agent-registry-control.{REGION}.amazonaws.com{path}"
        ))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(payload)
        .expect("a valid request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.rsplit('#').next().unwrap_or(value).to_string());
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        body,
        error_code,
    }
}

/// A client token of `ClientToken`'s shape (`@length 33..256`,
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`), derived from `seed` so each row uses its own.
fn token(seed: &str) -> String {
    let padded = format!("{seed}{}", "0".repeat(40));
    padded[..40].to_string()
}

fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

async fn create_registry(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "POST", "/registries", Some(body)).await
}

async fn get_registry(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(router, "GET", &format!("/registries/{registry_id}"), None).await
}

async fn create_record(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/records"),
        Some(body),
    )
    .await
}

/// How this service states that a registry name is already held, so a refusal for the NAME
/// can be told apart from a refusal for the TOKEN. Both are `ConflictException`, and only
/// the message says which rule refused. Written out here rather than read from the crate, so
/// the sentence a client receives is asserted independently of the code that builds it.
fn duplicate_name_message(name: &str) -> String {
    format!("A registry with name '{name}' already exists in this account")
}

/// Look at a registry until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn registry_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    wanted: &str,
) {
    let started = Instant::now();
    loop {
        let reply = get_registry(router, registry_id).await;
        if reply.str_field("status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `descriptors` value of `Descriptors`' own shape
/// (`smithy_query(query="shape", shape="Descriptors")`), for a `CUSTOM` record.
fn custom_descriptors(data: &str) -> Value {
    json!({ "custom": { "data": data } })
}

#[tokio::test]
async fn a_reused_create_registry_token_conflicts_on_a_nested_configuration_member() {
    // Rows differ along WHICH nested configuration structure of the create request the
    // second call changes — a required member of a structure, a boolean, a free-form
    // schema string, the length of a list, the order of a list's elements and an empty
    // versus a populated list. Every row keeps the registry NAME identical, so the only
    // thing that can refuse the second call is the token binding: a name clash would
    // answer the duplicate-name message instead of the fixed one asserted below.
    // Member names and enum values are the model's
    // (`smithy_query(query="shape", shape="EncryptionConfiguration,
    // AutoDetectionConfiguration,CustomMetadataSchemaConfiguration,ApprovalConfiguration,
    // AutoApprovalRule,AutoDetectionScope,RecordTypeSchemaOverride")`).
    //
    // The fourth element of each row is the member the row changed, so the registry the
    // first call minted can be read back and shown to still hold that call's value.
    let rows: Vec<(&str, Value, Value, &str)> = vec![
        (
            "the encryption key",
            json!({ "name": "nested-one", "encryptionConfiguration": { "kmsKeyArn": KMS_KEY_ONE } }),
            json!({ "name": "nested-one", "encryptionConfiguration": { "kmsKeyArn": KMS_KEY_TWO } }),
            "encryptionConfiguration",
        ),
        (
            "an auto-detection boolean",
            json!({ "name": "nested-two", "autoDetectionConfiguration": { "scope": "ORGANIZATION", "enabled": true } }),
            json!({ "name": "nested-two", "autoDetectionConfiguration": { "scope": "ORGANIZATION", "enabled": false } }),
            "autoDetectionConfiguration",
        ),
        (
            "a schema definition",
            json!({ "name": "nested-three", "customMetadataSchemaConfiguration": { "defaultSchema": "{\"type\":\"object\"}" } }),
            json!({ "name": "nested-three", "customMetadataSchemaConfiguration": { "defaultSchema": "{\"type\":\"object\",\"required\":[\"tier\"]}" } }),
            "customMetadataSchemaConfiguration",
        ),
        (
            "the length of a list",
            json!({
                "name": "nested-four",
                "customMetadataSchemaConfiguration": {
                    "recordTypeSchemaOverrides": [
                        { "recordType": "CUSTOM", "schema": "{\"type\":\"object\"}" }
                    ]
                }
            }),
            json!({
                "name": "nested-four",
                "customMetadataSchemaConfiguration": {
                    "recordTypeSchemaOverrides": [
                        { "recordType": "CUSTOM", "schema": "{\"type\":\"object\"}" },
                        { "recordType": "MCP", "schema": "{\"type\":\"object\"}" }
                    ]
                }
            }),
            "customMetadataSchemaConfiguration",
        ),
        (
            "the order of a list",
            json!({
                "name": "nested-five",
                "customMetadataSchemaConfiguration": {
                    "recordTypeSchemaOverrides": [
                        { "recordType": "CUSTOM", "schema": "{\"type\":\"object\"}" },
                        { "recordType": "MCP", "schema": "{\"type\":\"object\"}" }
                    ]
                }
            }),
            json!({
                "name": "nested-five",
                "customMetadataSchemaConfiguration": {
                    "recordTypeSchemaOverrides": [
                        { "recordType": "MCP", "schema": "{\"type\":\"object\"}" },
                        { "recordType": "CUSTOM", "schema": "{\"type\":\"object\"}" }
                    ]
                }
            }),
            "customMetadataSchemaConfiguration",
        ),
        (
            "an empty versus a populated list",
            json!({ "name": "nested-six", "approvalConfiguration": { "autoApprovalRules": [] } }),
            json!({ "name": "nested-six", "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] } }),
            "approvalConfiguration",
        ),
    ];

    for (case, first_body, second_body, member) in rows {
        let router = router();
        let client_token = token("nested-probe");
        let mut first = first_body.clone();
        first["clientToken"] = Value::from(client_token.clone());
        let mut second = second_body;
        second["clientToken"] = Value::from(client_token);

        let created = create_registry(&router, first.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the first create answers the modelled 202, got {} {:?}",
            created.status, created.body
        );
        let arn = created.str_field("registryArn").to_string();

        let refused = create_registry(&router, second).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: the same token with another parameter is the modelled 409, got \
             {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ConflictException"),
            "case {case:?}: the modelled error code is ConflictException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            PARAMETERS_DIFFER,
            "case {case:?}: the refusal is the token binding rather than the name, so the \
             message is the fixed conflict text, got {:?}",
            refused.message()
        );

        // Nothing of the refused request reached the store: the registry the first call
        // minted still holds that call's value for the member the second one changed,
        // read back through this service's own `GetRegistry`. The configuration a create
        // asks for is reported under its own member name, except the auto-detection one,
        // which `AutoDetection.configuration` carries
        // (`smithy_query(query="shape", shape="AutoDetection")`).
        let read_back = get_registry(&router, id_of(&arn)).await;
        let (reported, expected) = if member == "autoDetectionConfiguration" {
            (
                &read_back.body["autoDetection"]["configuration"],
                &first_body[member],
            )
        } else {
            (&read_back.body[member], &first_body[member])
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: the refused create stored nothing, so {member} is still the \
             first call's, got {} {:?}",
            read_back.status, read_back.body
        );

        let replay = create_registry(&router, first).await;
        assert_eq!(
            (replay.status, replay.str_field("registryArn")),
            (202, arn.as_str()),
            "case {case:?}: the original request still replays its own answer, got {} {:?}",
            replay.status,
            replay.body
        );
    }
}

#[tokio::test]
async fn a_client_token_is_case_sensitive_and_bound_per_operation() {
    let router = router();
    // The model documents `clientToken` as a "case-sensitive identifier", so two tokens
    // differing only in case are two tokens.
    let lower = token("idemcasetoken");
    let upper = lower.to_uppercase();
    // The seed has to carry a letter for the pair to differ at all, which is what makes the
    // case the row is about.
    assert_ne!(
        lower, upper,
        "case \"the token pair\": the two tokens must differ only in case, got {lower:?} and \
         {upper:?}"
    );

    let first = create_registry(
        &router,
        json!({ "name": "case-one", "description": "lower", "clientToken": lower.clone() }),
    )
    .await;
    assert_eq!(
        first.status, 202,
        "case \"the lower-case token\": the create answers 202, got {} {:?}",
        first.status, first.body
    );
    let first_arn = first.str_field("registryArn").to_string();

    // The same text in another case is a token of its own, so this is a first call and
    // creates: it is neither replayed nor refused.
    let other_case = create_registry(
        &router,
        json!({ "name": "case-two", "description": "upper", "clientToken": upper.clone() }),
    )
    .await;
    assert_eq!(
        other_case.status, 202,
        "case \"the upper-case token\": a token differing only in case is a different token, \
         so the create answers 202, got {} {:?}",
        other_case.status, other_case.body
    );
    assert_ne!(
        other_case.str_field("registryArn"),
        first_arn.as_str(),
        "case \"the upper-case token\": it must mint its own registry rather than replay the \
         other token's answer, got {:?}",
        other_case.body
    );

    // And the lower-case token still names what it first created.
    let replay = create_registry(
        &router,
        json!({ "name": "case-one", "description": "lower", "clientToken": lower.clone() }),
    )
    .await;
    assert_eq!(
        (replay.status, replay.str_field("registryArn")),
        (202, first_arn.as_str()),
        "case \"the lower-case token\": the first request still replays its own answer, got \
         {} {:?}",
        replay.status,
        replay.body
    );

    let registry_id = id_of(&first_arn).to_string();
    registry_reaching(&router, "the record's registry", &registry_id, "READY").await;

    // A token is bound per OPERATION: the text that names a CreateRegistry call says
    // nothing about a CreateRegistryRecord call, so presenting it to the record create —
    // with parameters that have nothing in common with the registry create's — is a first
    // call there.
    let record_body = json!({
        "name": "rec-shared-token",
        "recordType": "CUSTOM",
        "descriptors": custom_descriptors("{\"a\":1}"),
        "clientToken": lower.clone()
    });
    let record = create_record(&router, &registry_id, record_body.clone()).await;
    assert_eq!(
        record.status, 202,
        "case \"the same token on another operation\": the record create answers the modelled \
         202, got {} {:?}",
        record.status, record.body
    );
    let record_arn = record.str_field("recordArn").to_string();

    // Within the record create the same token is now bound to those parameters: the same
    // request replays, a changed one is the modelled conflict.
    let record_replay = create_record(&router, &registry_id, record_body).await;
    assert_eq!(
        (record_replay.status, record_replay.str_field("recordArn")),
        (202, record_arn.as_str()),
        "case \"the record token replayed\": got {} {:?}",
        record_replay.status,
        record_replay.body
    );
    let record_refused = create_record(
        &router,
        &registry_id,
        json!({
            "name": "rec-shared-token",
            "recordType": "CUSTOM",
            "descriptors": custom_descriptors("{\"a\":2}"),
            "clientToken": lower
        }),
    )
    .await;
    assert_eq!(
        (
            record_refused.status,
            record_refused.error_code.as_deref(),
            record_refused.message().as_str()
        ),
        (409, Some("ConflictException"), PARAMETERS_DIFFER),
        "case \"the record token reused\": the same token with other parameters is the \
         modelled 409 with the fixed text, got {} {:?}",
        record_refused.status,
        record_refused.body
    );
}

#[tokio::test]
async fn a_client_token_binding_outlives_the_creation_the_clock_owes() {
    // Creation is asynchronous, so the registry a token names changes status after the
    // call that minted it. The binding is a property of the request, not of the status the
    // resource happens to be in, so it holds on both sides of that transition. Rows differ
    // along WHEN the second call arrives: while the registry is still being created, and
    // once it has reached READY.
    let rows: [(&str, bool); 2] = [("while still CREATING", false), ("once READY", true)];

    for (case, wait_for_ready) in rows {
        let router = router();
        let client_token = token("outlives-probe");
        let request = json!({
            "name": "outlives",
            "description": "the first parameters",
            "tags": { "team": "platform" },
            "clientToken": client_token.clone()
        });

        let created = create_registry(&router, request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the first create answers 202, got {} {:?}",
            created.status, created.body
        );
        let arn = created.str_field("registryArn").to_string();
        if wait_for_ready {
            registry_reaching(&router, case, id_of(&arn), "READY").await;
        }

        let replay = create_registry(&router, request).await;
        assert_eq!(
            (replay.status, replay.str_field("registryArn")),
            (202, arn.as_str()),
            "case {case:?}: the same request replays the ARN the first call minted, got {} {:?}",
            replay.status,
            replay.body
        );

        let refused = create_registry(
            &router,
            json!({
                "name": "outlives",
                "description": "other parameters",
                "tags": { "team": "platform" },
                "clientToken": client_token
            }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message().as_str()
            ),
            (409, Some("ConflictException"), PARAMETERS_DIFFER),
            "case {case:?}: the same token with other parameters is the modelled 409 with the \
             fixed text, got {} {:?}",
            refused.status,
            refused.body
        );

        // Neither the replay nor the refusal created a second registry: the name is still
        // held by one registry, so a create presenting a FRESH token and that name is
        // refused for the NAME — the other conflict, told apart by its message.
        let name_clash = create_registry(
            &router,
            json!({ "name": "outlives", "clientToken": token("a-fresh-token-for-the-name") }),
        )
        .await;
        assert_eq!(
            (
                name_clash.status,
                name_clash.error_code.as_deref(),
                name_clash.message()
            ),
            (
                409,
                Some("ConflictException"),
                duplicate_name_message("outlives")
            ),
            "case {case:?}: the name is still held, so a fresh token is refused for the name, \
             got {} {:?}",
            name_clash.status,
            name_clash.body
        );

        let read_back = get_registry(&router, id_of(&arn)).await;
        assert_eq!(
            (read_back.status, read_back.str_field("description")),
            (200, "the first parameters"),
            "case {case:?}: the registry still holds the parameters of the call that minted \
             it, got {} {:?}",
            read_back.status,
            read_back.body
        );
    }
}
