// SPDX-License-Identifier: CC0-1.0

//! The precondition `DeleteRegistry` enforces, driven over HTTP through the real router so
//! every assertion reads the bytes a client would.
//!
//! The rule under test: a container resource is deleted only once it is empty — while any
//! child resource still lives under it the delete is refused with the modelled
//! `ConflictException` (`smithy_query(query="operation",
//! operation="DeleteRegistry")`: errorKind client, httpError 409) and the container is left
//! exactly as it was, and once the last child is gone the delete is accepted (HTTP 202,
//! `status` `DELETING`) and the container is removed.
//!
//! The rule is about the registry's CONTENTS, not about one record's shape, so the tests
//! assert it over a TABLE spanning what it generalises over: how many records the registry
//! holds, which `RecordType` each one is (`smithy_query(query="shape",
//! shape="RecordType")`), which `RegistryRecordStatus` they have reached, and whether the
//! registry is addressed by its bare id or by its ARN (both of which
//! `RegistryIdentifier`'s pattern admits).

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

/// How long the poll loop pauses between two looks at the resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

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

    fn message(&self) -> &str {
        self.str_field("message")
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

/// Percent-encode a path segment the way an SDK encodes a non-greedy `@httpLabel`, so a
/// registry can also be addressed by its ARN (which carries `:` and `/`).
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

async fn create_registry(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "POST", "/registries", Some(body)).await
}

async fn get_registry(router: &MultiServiceRouter, identifier: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/registries/{}", encode_label(identifier)),
        None,
    )
    .await
}

async fn delete_registry(router: &MultiServiceRouter, identifier: &str) -> Reply {
    call(
        router,
        "DELETE",
        &format!("/registries/{}", encode_label(identifier)),
        None,
    )
    .await
}

async fn create_record(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(body),
    )
    .await
}

async fn get_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

async fn delete_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "DELETE",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

async fn submit_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!(
            "/registries/{}/records/{}/submit-for-approval",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        None,
    )
    .await
}

/// Look at a registry until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn registry_reaching(router: &MultiServiceRouter, case: &str, registry_id: &str, wanted: &str) {
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

/// Look at a record until it reports `wanted`, or give up at the deadline.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) {
    let started = Instant::now();
    loop {
        let reply = get_record(router, registry_id, record_id).await;
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

/// Look at a registry until it is gone, or give up at the deadline: an accepted delete is
/// asynchronous, so the removal is owed by the clock.
async fn registry_gone(router: &MultiServiceRouter, case: &str, registry_id: &str) {
    let started = Instant::now();
    loop {
        let reply = get_registry(router, registry_id).await;
        if reply.status == 404 {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry was never removed, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a registry from `body` and wait for the creation the clock owes. Returns its
/// registry id and the ARN the create answered, which is the ARN a refusal must quote.
async fn ready_registry(router: &MultiServiceRouter, case: &str, body: Value) -> (String, String) {
    let created = create_registry(router, body).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry answers 202, got {} {:?}",
        created.status, created.body
    );
    let registry_arn = created.str_field("registryArn").to_string();
    let registry_id = id_of(&registry_arn).to_string();
    registry_reaching(router, case, &registry_id, "READY").await;
    (registry_id, registry_arn)
}

/// An MCP server definition a supported MCP server schema version accepts: the MCP server
/// descriptor's content is validated against that schema, so a record of this type carries a
/// named server with a description and a version.
const MCP_SERVER_DEFINITION: &str =
    r#"{"name":"com.example/checkout","description":"Checkout tools","version":"1.4.2"}"#;

/// A `descriptors` value per `RecordType`, so each row of a record table carries a
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

/// Create a record of `record_type` and wait for the creation the clock owes, so it is
/// `DRAFT`. Returns its record id.
async fn draft_record(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    name: &str,
    record_type: &str,
) -> String {
    let created = create_record(
        router,
        registry_id,
        json!({
            "name": name,
            "recordType": record_type,
            "descriptors": descriptors_for(record_type),
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
        created.status, created.body
    );
    let record_id = id_of(created.str_field("recordArn")).to_string();
    record_reaching(router, case, registry_id, &record_id, "DRAFT").await;
    record_id
}

/// The sentence a delete of a non-empty registry is refused with: fixed text whose one
/// variable part is the registry's own ARN, as the emulator's state holds it.
fn conflict_message(registry_arn: &str) -> String {
    format!(
        "Cannot delete registry '{registry_arn}' because it contains registry records. \
         Delete all registry records before deleting the registry."
    )
}

/// Assert `reply` is the modelled refusal of a delete of the registry at `registry_arn`.
fn assert_refused(case: &str, reply: &Reply, registry_arn: &str) {
    assert_eq!(
        reply.status, 409,
        "case {case:?}: a registry holding records refuses the delete with 409, got {} {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("ConflictException"),
        "case {case:?}: expected ConflictException, got {:?}",
        reply.error_code
    );
    assert_eq!(
        reply.message(),
        conflict_message(registry_arn),
        "case {case:?}: expected {:?}, got {:?}",
        conflict_message(registry_arn),
        reply.message()
    );
}

/// One row of the table below: the registry to create, the records it comes to hold, and
/// how the delete addresses it.
struct Case {
    case: &'static str,
    /// The `CreateRegistry` body.
    registry: Value,
    /// `(name, recordType)` of each record created in the registry.
    records: Vec<(&'static str, &'static str)>,
    /// Submit each record for approval first, so the row's records sit past `DRAFT`.
    submit: bool,
    /// Address the registry by its ARN rather than by its bare id.
    by_arn: bool,
}

#[tokio::test]
async fn delete_registry_is_refused_until_the_last_record_under_it_is_gone() {
    let router = router();
    // Rows differ along how many records the registry holds, which record types they are,
    // which status they have reached, and whether the delete addresses the registry by its
    // bare id or by its ARN. `submit` sends each record for approval first, so the row's
    // records sit past `DRAFT`; the registry of the last row auto-approves a submission
    // (`APPROVE_ALL`), so that row's record reaches `APPROVED` rather than
    // `PENDING_APPROVAL`.
    let cases = vec![
        Case {
            case: "one CUSTOM record, registry by id",
            registry: json!({ "name": "holds-one-record" }),
            records: vec![("only-record", "CUSTOM")],
            submit: false,
            by_arn: false,
        },
        Case {
            case: "a record of every modelled type, registry by ARN",
            registry: json!({ "name": "holds-many-records" }),
            records: vec![
                ("mcp-record", "MCP"),
                ("agent-record", "AGENT"),
                ("custom-record", "CUSTOM"),
                ("skill-record", "SKILL"),
                ("gateway-record", "GATEWAY"),
            ],
            submit: false,
            by_arn: true,
        },
        Case {
            case: "a record awaiting manual approval, registry by id",
            registry: json!({ "name": "holds-pending-record" }),
            records: vec![("pending-record", "SKILL")],
            submit: true,
            by_arn: false,
        },
        Case {
            case: "an auto-approved record, registry by ARN",
            registry: json!({
                "name": "holds-approved-record",
                "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] }
            }),
            records: vec![("approved-record", "MCP")],
            submit: true,
            by_arn: true,
        },
    ];

    for Case {
        case,
        registry,
        records,
        submit,
        by_arn,
    } in cases
    {
        let (registry_id, registry_arn) = ready_registry(&router, case, registry).await;
        let mut record_ids = Vec::new();
        for (name, record_type) in &records {
            let record_id = draft_record(&router, case, &registry_id, name, record_type).await;
            if submit {
                let submitted = submit_record(&router, &registry_id, &record_id).await;
                assert_eq!(
                    submitted.status, 202,
                    "case {case:?}: SubmitRegistryRecordForApproval answers 202, got {} {:?}",
                    submitted.status, submitted.body
                );
            }
            record_ids.push(record_id);
        }
        let label = if by_arn {
            registry_arn.clone()
        } else {
            registry_id.clone()
        };

        let before = get_registry(&router, &registry_id).await;
        let refused = delete_registry(&router, &label).await;
        assert_refused(case, &refused, &registry_arn);

        // The refusal mutates nothing: the whole read-back is what it was, so the status is
        // still READY and `updatedAt` was not bumped either.
        let after = get_registry(&router, &registry_id).await;
        assert_eq!(
            after.str_field("status"),
            "READY",
            "case {case:?}: the refused delete leaves the registry READY, got {:?}",
            after.body
        );
        assert_eq!(
            after.body, before.body,
            "case {case:?}: the refused delete left the registry changed, {:?} became {:?}",
            before.body, after.body
        );

        // One record left is still a non-empty registry: the delete is refused until the
        // last of them is gone, and is then accepted.
        let total = record_ids.len();
        for (index, record_id) in record_ids.iter().enumerate() {
            let deleted = delete_record(&router, &registry_id, record_id).await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: DeleteRegistryRecord answers 200, got {} {:?}",
                deleted.status, deleted.body
            );
            let reply = delete_registry(&router, &label).await;
            if index + 1 < total {
                assert_refused(
                    &format!("{case} with {} records left", total - index - 1),
                    &reply,
                    &registry_arn,
                );
            } else {
                assert_eq!(
                    reply.status, 202,
                    "case {case:?}: an emptied registry is deleted with 202, got {} {:?}",
                    reply.status, reply.body
                );
                assert_eq!(
                    reply.str_field("status"),
                    "DELETING",
                    "case {case:?}: deletion is asynchronous, so the status is DELETING, got {:?}",
                    reply.body
                );
            }
        }

        registry_gone(&router, case, &registry_id).await;
    }
}

#[tokio::test]
async fn a_record_that_is_still_being_created_blocks_the_delete_too() {
    // A record counts from the instant its create is accepted, whatever status it has
    // reached: the registry is not empty while a record of it is still being created.
    let case = "a record still being created";
    let router = router();
    let (registry_id, registry_arn) =
        ready_registry(&router, case, json!({ "name": "holds-a-fresh-record" })).await;
    let created = create_record(
        &router,
        &registry_id,
        json!({
            "name": "fresh-record",
            "recordType": "AGENT",
            "descriptors": descriptors_for("AGENT"),
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
        created.status, created.body
    );
    let record_id = id_of(created.str_field("recordArn")).to_string();

    let refused = delete_registry(&router, &registry_id).await;
    assert_refused(case, &refused, &registry_arn);

    // The record finishes being created and is then deleted, which empties the registry.
    record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
    let deleted = delete_record(&router, &registry_id, &record_id).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeleteRegistryRecord answers 200, got {} {:?}",
        deleted.status, deleted.body
    );
    let accepted = delete_registry(&router, &registry_id).await;
    assert_eq!(
        accepted.status, 202,
        "case {case:?}: an emptied registry is deleted with 202, got {} {:?}",
        accepted.status, accepted.body
    );
    registry_gone(&router, case, &registry_id).await;
}

#[tokio::test]
async fn an_empty_registry_is_deleted_without_a_conflict() {
    let router = router();
    // Rows differ along how the registry came to be empty — it never held a record, or it
    // held records that were all deleted — and along how the delete addresses it.
    let cases: [(&str, &str, usize, bool); 4] = [
        ("never held a record, by id", "empty-by-id", 0, false),
        ("never held a record, by ARN", "empty-by-arn", 0, true),
        ("emptied of one record", "emptied-of-one", 1, false),
        ("emptied of three records", "emptied-of-three", 3, true),
    ];

    for (case, name, held, by_arn) in cases {
        let (registry_id, registry_arn) =
            ready_registry(&router, case, json!({ "name": name })).await;
        for index in 0..held {
            let record_id =
                draft_record(&router, case, &registry_id, &format!("record-{index}"), "CUSTOM")
                    .await;
            let deleted = delete_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: DeleteRegistryRecord answers 200, got {} {:?}",
                deleted.status, deleted.body
            );
        }

        let label = if by_arn { &registry_arn } else { &registry_id };
        let accepted = delete_registry(&router, label).await;
        assert_eq!(
            accepted.status, 202,
            "case {case:?}: an empty registry is deleted with 202, got {} {:?}",
            accepted.status, accepted.body
        );
        assert_eq!(
            accepted.str_field("status"),
            "DELETING",
            "case {case:?}: deletion is asynchronous, so the status is DELETING, got {:?}",
            accepted.body
        );
        assert_eq!(
            accepted.error_code, None,
            "case {case:?}: an empty registry's delete carries no error code, got {:?}",
            accepted.error_code
        );

        // @idempotent: a second delete of a registry already on its way out is accepted and
        // reports the same status, rather than conflicting.
        let again = delete_registry(&router, label).await;
        assert_eq!(
            again.status, 202,
            "case {case:?}: a repeated delete is accepted, got {} {:?}",
            again.status, again.body
        );
        assert_eq!(
            again.str_field("status"),
            "DELETING",
            "case {case:?}: the status is unchanged, got {:?}",
            again.body
        );

        registry_gone(&router, case, &registry_id).await;
    }
}
