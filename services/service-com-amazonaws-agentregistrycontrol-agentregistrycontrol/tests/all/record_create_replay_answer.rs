// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a create answers when its `@idempotencyToken` is REPLAYED,
//! driven over HTTP through the real router so every assertion goes through routing,
//! deserialization, the handler and serialization.
//!
//! The rule under test: a create whose `clientToken` matches a remembered call "ignores the
//! request", so it answers the members that call answered — including the transient status
//! an asynchronous create reports — however far the resource has moved on since. A replay
//! is therefore indistinguishable from the first call, whether it arrives while the
//! creation is still in flight or long after the record has settled, been submitted or been
//! retired.
//!
//! Every test asserts over a TABLE of inputs, so it holds for a class of requests rather
//! than for the one body a recording happened to carry. The rows differ along the status the
//! record has reached by the time the replay arrives, and along the record type (hence the
//! descriptor shape) the token was first used with.

use std::sync::Arc;
use std::time::Duration;

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

/// How long a wait on a transition the clock owes may take before the test gives up.
/// Generous: the wait is on the CONDITION, so this bounds only a hang.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long to wait between two looks while polling for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The `RegistryRecordStatus` an asynchronous create reports while it is in flight; the
/// catalogue is the model's own `RegistryRecordStatus`
/// (`smithy_query(query="shape", shape="RegistryRecordStatus")`).
const CREATING: &str = "CREATING";

/// One HTTP answer, reduced to what these tests assert on.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
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
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// Percent-encode a path segment the way an SDK encodes a non-greedy `@httpLabel`.
fn encode_label(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// A client token of `ClientToken`'s shape (`@length 33..256`,
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`), derived from `seed` so each row uses its own.
fn token(seed: &str) -> String {
    let sanitized: String = seed
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    format!("{sanitized}{}", "0".repeat(64))[..40].to_string()
}

/// An MCP server definition a supported MCP server schema version accepts: the MCP server
/// descriptor's content is validated against that schema, so a record of this type carries a
/// named server with a description and a version.
const MCP_SERVER_DEFINITION: &str =
    r#"{"name":"com.example/checkout","description":"Checkout tools","version":"1.4.2"}"#;

/// A `descriptors` value per `RecordType`, so each row of the record table carries a
/// descriptor of its own shape. Member names are `Descriptors`' own
/// (`smithy_query(query="shape", shape="Descriptors")`).
fn descriptors_for(record_type: &str) -> Value {
    match record_type {
        "MCP" => {
            json!({ "mcpServer": { "data": MCP_SERVER_DEFINITION, "dataSchemaVersion": "2025-06-18" } })
        }
        "AGENT" => {
            json!({ "a2aAgentCard": { "data": "{\"name\":\"checkout\"}", "dataSchemaVersion": "0.2.1" } })
        }
        "SKILL" => json!({ "agentSkillsDefinition": { "data": "{\"skills\":[]}" } }),
        "GATEWAY" => {
            json!({ "http": { "source": { "fromUrl": { "url": "https://example.test/gateway" } } } })
        }
        _ => json!({ "custom": { "data": "{\"anything\":true}" } }),
    }
}

async fn get_record(router: &MultiServiceRouter, registry: &str, record: &str) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry),
            encode_label(record)
        ),
        None,
    )
    .await
}

/// Look at a record until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry: &str,
    record: &str,
    wanted: &str,
) {
    let started = std::time::Instant::now();
    loop {
        let reply = get_record(router, registry, record).await;
        if reply.str_field("status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the record never reached {wanted}, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `READY` registry, created through this service's own operations.
async fn ready_registry(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = call(router, "POST", "/registries", Some(json!({ "name": name }))).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry must answer 202, got {} {:?}",
        created.status, created.body
    );
    let registry_id = id_of(created.str_field("registryArn")).to_string();

    let started = std::time::Instant::now();
    loop {
        let reply = call(
            router,
            "GET",
            &format!("/registries/{}", encode_label(&registry_id)),
            None,
        )
        .await;
        if reply.str_field("status") == "READY" {
            return registry_id;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry never became READY, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn create_record(router: &MultiServiceRouter, registry: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry)),
        Some(body),
    )
    .await
}

/// What a row does to the record between the first create and the replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveOn {
    /// Nothing: the replay arrives while the creation may still be in flight.
    Nothing,
    /// Wait for the creation the clock owes, so the record has settled at `DRAFT`.
    Settle,
    /// Settle, then submit the record, so it is in the approval workflow.
    Submit,
    /// Settle, then retire the record, so it is in the lifecycle's terminal status.
    Retire,
}

#[tokio::test]
async fn a_replayed_record_create_answers_the_first_call_s_members() {
    // Rows differ along how far the record has moved on by the time the replay arrives —
    // including statuses only this service's own work and the curation workflow reach — and
    // along the record type the token was used with, so the rule is not read off one shape.
    let rows: Vec<(&str, &str, MoveOn)> = vec![
        ("creation still in flight", "CUSTOM", MoveOn::Nothing),
        ("settled at DRAFT", "CUSTOM", MoveOn::Settle),
        ("settled at DRAFT, MCP record", "MCP", MoveOn::Settle),
        ("submitted for approval", "AGENT", MoveOn::Submit),
        ("retired", "SKILL", MoveOn::Retire),
        ("source-only descriptor", "GATEWAY", MoveOn::Settle),
    ];

    for (case, record_type, move_on) in rows {
        let router = router();
        let registry_id = ready_registry(&router, case, &format!("replay-{}", token(case))).await;
        let client_token = token(case);
        let request = json!({
            "name": format!("rec-{}", token(case)),
            "recordType": record_type,
            "descriptors": descriptors_for(record_type),
            "clientToken": client_token,
        });

        let created = create_record(&router, &registry_id, request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the first create answers the modelled 202, got {} {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.str_field("status"),
            CREATING,
            "case {case:?}: an asynchronous create reports {CREATING}, got {:?}",
            created.body
        );
        let record_arn = created.str_field("recordArn").to_string();
        let record_id = id_of(&record_arn).to_string();

        match move_on {
            MoveOn::Nothing => {}
            MoveOn::Settle => {
                record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
            }
            MoveOn::Submit => {
                record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
                let submitted = call(
                    &router,
                    "POST",
                    &format!(
                        "/registries/{}/records/{}/submit-for-approval",
                        encode_label(&registry_id),
                        encode_label(&record_id)
                    ),
                    None,
                )
                .await;
                assert_eq!(
                    submitted.status, 202,
                    "case {case:?}: the submission answers 202, got {} {:?}",
                    submitted.status, submitted.body
                );
            }
            MoveOn::Retire => {
                record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
                let retired = call(
                    &router,
                    "PATCH",
                    &format!(
                        "/registries/{}/records/{}/status",
                        encode_label(&registry_id),
                        encode_label(&record_id)
                    ),
                    Some(json!({ "status": "DEPRECATED", "statusReason": "retired by the suite" })),
                )
                .await;
                assert_eq!(
                    retired.status, 202,
                    "case {case:?}: retiring the record answers 202, got {} {:?}",
                    retired.status, retired.body
                );
            }
        }

        // The record's own status now reports where it has got to, which for every row but
        // the first is NOT the status the create answered.
        let now = get_record(&router, &registry_id, &record_id).await;
        if move_on != MoveOn::Nothing {
            assert_ne!(
                now.str_field("status"),
                CREATING,
                "case {case:?}: the record must have moved on before the replay, got {:?}",
                now.body
            );
        }

        let replay = create_record(&router, &registry_id, request).await;
        assert_eq!(
            replay.status, 202,
            "case {case:?}: a replay is not an error, got {} {:?}",
            replay.status, replay.body
        );
        assert_eq!(
            replay.str_field("recordArn"),
            record_arn,
            "case {case:?}: a replay answers the ARN the first call minted, got {:?}",
            replay.body
        );
        assert_eq!(
            replay.str_field("status"),
            CREATING,
            "case {case:?}: a replay answers the status the first call reported, got {:?}",
            replay.body
        );

        // The replay created nothing and changed nothing: the record is still the one the
        // first call made, in the status it had reached.
        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (after.status, after.str_field("status")),
            (200, now.str_field("status")),
            "case {case:?}: the replay left the record alone, got {} {:?}",
            after.status,
            after.body
        );
        let listed = call(
            &router,
            "POST",
            &format!("/registries/{}/records-list", encode_label(&registry_id)),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            listed.body["registryRecords"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: the replay created no second record, got {:?}",
            listed.body
        );
    }
}

#[tokio::test]
async fn a_token_whose_record_is_gone_creates_a_new_one() {
    // The remembered answer names a record: once that record has been deleted the token
    // names nothing, so the same request is a FIRST call again and mints a new ARN. Rows
    // differ along the record type, so the rule is not read off one descriptor shape.
    for record_type in ["CUSTOM", "MCP", "SKILL"] {
        let case = format!("deleted {record_type} record");
        let router = router();
        let registry_id = ready_registry(&router, &case, &format!("gone-{}", token(&case))).await;
        let request = json!({
            "name": format!("rec-{}", token(&case)),
            "recordType": record_type,
            "descriptors": descriptors_for(record_type),
            "clientToken": token(&case),
        });

        let created = create_record(&router, &registry_id, request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: the first create answers 202, got {} {:?}",
            created.status, created.body
        );
        let first_arn = created.str_field("recordArn").to_string();
        let record_id = id_of(&first_arn).to_string();
        record_reaching(&router, &case, &registry_id, &record_id, "DRAFT").await;

        let deleted = call(
            &router,
            "DELETE",
            &format!(
                "/registries/{}/records/{}",
                encode_label(&registry_id),
                encode_label(&record_id)
            ),
            None,
        )
        .await;
        assert!(
            (200..300).contains(&deleted.status),
            "case {case:?}: the delete must succeed, got {} {:?}",
            deleted.status,
            deleted.body
        );

        let again = create_record(&router, &registry_id, request).await;
        assert_eq!(
            again.status, 202,
            "case {case:?}: the create runs again, got {} {:?}",
            again.status, again.body
        );
        assert_ne!(
            again.str_field("recordArn"),
            first_arn,
            "case {case:?}: a token naming a deleted record mints a new one, got {:?}",
            again.body
        );
        assert_eq!(
            again.str_field("status"),
            CREATING,
            "case {case:?}: the new record is reported {CREATING}, got {:?}",
            again.body
        );
    }
}
