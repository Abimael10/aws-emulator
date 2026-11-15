// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateRegistryRecordStatus`, driven over HTTP through the real
//! router so every assertion goes through routing, deserialization, the handler and
//! serialization.
//!
//! The rule under test: `UpdateRegistryRecordStatus` moves a record to the status a
//! curator names and records the reason given with it — "Updates the status of a registry
//! record as part of the registry's curation workflow, for example to approve or reject a
//! record that is pending approval, or to deprecate an approved record so that it is no
//! longer discoverable". The answer echoes the resulting status and that reason, and both
//! are persisted, so every later read reports them. The statuses a curator may name are
//! the curation ones: a status that reports an ASYNCHRONOUS change this service makes
//! itself (`CREATING`, `UPDATING`, and the two `*_FAILED` values) is not one a record can
//! be moved to, and a record in one of them is not in the workflow yet — both are the
//! modelled `ConflictException`. A registry or a record this service does not hold is the
//! modelled `ResourceNotFoundException`, worded as every other operation of this service
//! words that class.
//!
//! Every test asserts over a TABLE of inputs, so it holds for a class of requests rather
//! than for one recorded shape: the rows span the status a record starts in, the status it
//! is moved to, the reason text carried with it, the identifier form each label is given
//! in (bare id and full ARN), and which of the two resources is absent.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::state as control;
use smithy_server_runtime::MultiServiceRouter;

/// The region in the test's SigV4 credential scope; every minted ARN carries it.
const REGION: &str = "us-west-2";

/// A signed-looking request, so the router resolves this service's signing name
/// (`agent-registry`) and the handlers read the region from the credential scope. The
/// signature itself is never checked: the test router runs with authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// How long a wait for a transition the clock owes may take before a test gives up.
/// Generous on purpose: the wait is on the CONDITION, so a loaded host only polls more.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The four `RegistryRecordStatus` values that report an asynchronous change this service
/// makes itself rather than a step of the curation workflow; the whole enum is
/// `crate::ops::types::REGISTRY_RECORD_STATUS_VALUES` (APPROVED, CREATE_FAILED, CREATING,
/// DEPRECATED, DRAFT, PENDING_APPROVAL, REJECTED, UPDATE_FAILED, UPDATING).
const ASYNCHRONOUS_STATUSES: [&str; 4] = ["CREATING", "UPDATING", "CREATE_FAILED", "UPDATE_FAILED"];

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

/// The router and the engine behind it, so a test can put a record into a status only this
/// service's own asynchronous work reaches.
fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
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

/// Percent-encode a path segment the way an SDK encodes a non-greedy `@httpLabel`, so an
/// ARN (which carries `:` and `/`) can be given where the model allows the ARN form.
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

async fn update_status(
    router: &MultiServiceRouter,
    registry: &str,
    record: &str,
    body: Value,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!(
            "/registries/{}/records/{}/status",
            encode_label(registry),
            encode_label(record)
        ),
        Some(body),
    )
    .await
}

/// Look at a record until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes — never on a fixed span.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) {
    let started = std::time::Instant::now();
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

/// A `READY` registry holding one record in `start_status`, created through this service's
/// own operations and taken to that status through them — `auto_approve` chooses what a
/// submission lands in. Returns (registryId, registryArn, recordId, recordArn).
async fn record_in_status(
    router: &MultiServiceRouter,
    case: &str,
    auto_approve: bool,
    start_status: &str,
) -> (String, String, String, String) {
    let registry_name: String = case
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let mut registry_body = json!({ "name": registry_name });
    if auto_approve {
        registry_body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
    }
    let created = call(router, "POST", "/registries", Some(registry_body)).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry must answer 202, got {} {:?}",
        created.status, created.body
    );
    let registry_arn = created.str_field("registryArn").to_string();
    let registry_id = id_of(&registry_arn).to_string();

    // A record can only be created in a registry that has finished being created.
    let started = std::time::Instant::now();
    let record = loop {
        let attempt = call(
            router,
            "POST",
            &format!("/registries/{}/records", encode_label(&registry_id)),
            Some(json!({
                "name": "curation-probe",
                "recordType": "CUSTOM",
                "recordVersion": "1.0.0",
                "descriptors": { "custom": { "data": "{\"anything\":true}" } },
            })),
        )
        .await;
        if attempt.status == 202 {
            break attempt;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: CreateRegistryRecord never succeeded, last answer {} {:?}",
            attempt.status,
            attempt.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let record_arn = record.str_field("recordArn").to_string();
    let record_id = id_of(&record_arn).to_string();
    record_reaching(router, case, &registry_id, &record_id, "DRAFT").await;

    if start_status != "DRAFT" {
        let submitted = call(
            router,
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
            "case {case:?}: SubmitRegistryRecordForApproval must answer 202, got {} {:?}",
            submitted.status, submitted.body
        );
        record_reaching(router, case, &registry_id, &record_id, start_status).await;
    }
    (registry_id, registry_arn, record_id, record_arn)
}

#[tokio::test]
async fn a_curation_status_is_written_with_its_reason_and_read_back() {
    let (router, _engine) = router();

    // (case, the status the record starts in, whether its registry auto-approves, the
    // status a curator moves it to, the reason given). The rows span the curation
    // transitions the operation's own doc names — approve or reject a record that is
    // pending approval, deprecate an approved one — plus a move back out of the workflow,
    // and they differ in the reason text so no fixed string can satisfy them.
    let rows: [(&str, &str, bool, &str, &str); 5] = [
        (
            "approve a record pending approval",
            "PENDING_APPROVAL",
            false,
            "APPROVED",
            "reviewed by the catalogue team",
        ),
        (
            "reject a record pending approval",
            "PENDING_APPROVAL",
            false,
            "REJECTED",
            "descriptor does not describe a reachable endpoint",
        ),
        (
            "deprecate an approved record",
            "APPROVED",
            true,
            "DEPRECATED",
            "superseded by v2",
        ),
        (
            "return an approved record to draft",
            "APPROVED",
            true,
            "DRAFT",
            "withdrawn at the owner's request",
        ),
        (
            "move a draft record into the workflow",
            "DRAFT",
            false,
            "PENDING_APPROVAL",
            "",
        ),
    ];

    for (case, start_status, auto_approve, target, reason) in rows {
        let (registry_id, registry_arn, record_id, record_arn) =
            record_in_status(&router, case, auto_approve, start_status).await;

        let updated = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": target, "statusReason": reason }),
        )
        .await;
        assert_eq!(
            updated.status, 202,
            "case {case:?}: UpdateRegistryRecordStatus must answer 202, got {} {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            (
                updated.str_field("status"),
                updated.str_field("statusReason"),
                updated.str_field("recordId"),
                updated.str_field("recordArn"),
                updated.str_field("registryArn"),
            ),
            (
                target,
                reason,
                record_id.as_str(),
                record_arn.as_str(),
                registry_arn.as_str(),
            ),
            "case {case:?}: the answer reports the resulting status, the reason it was \
             given and the record it addressed, got {:?}",
            updated.body
        );

        // The write is what a later read reports: the status AND the reason.
        let read_back = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (
                read_back.status,
                read_back.str_field("status"),
                read_back.str_field("statusReason"),
            ),
            (200, target, reason),
            "case {case:?}: a read after the transition reports it, got {} {:?}",
            read_back.status,
            read_back.body
        );

        // Either identifier form names the same record, so the ARN form reaches it too.
        let again = update_status(
            &router,
            &registry_arn,
            &record_arn,
            json!({ "status": start_status, "statusReason": "restored by the suite" }),
        )
        .await;
        assert_eq!(
            (again.status, again.str_field("status")),
            (202, start_status),
            "case {case:?}: the ARN form of each label names the same resources, got {} {:?}",
            again.status,
            again.body
        );
    }
}

#[tokio::test]
async fn an_asynchronous_status_is_not_one_a_curator_can_name() {
    let (router, engine) = router();
    let case = "asynchronous target status";
    let (registry_id, _registry_arn, record_id, _record_arn) =
        record_in_status(&router, case, true, "APPROVED").await;

    // A target status that reports this service's own asynchronous work is refused, and
    // the record keeps the status it had.
    for target in ASYNCHRONOUS_STATUSES {
        let refused = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": target, "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (409, Some("ConflictException")),
            "case {case:?} ({target}): expected the modelled 409 ConflictException, got {} \
             {:?}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(target),
            "case {case:?} ({target}): the refusal names the status it would not accept, \
             got {:?}",
            refused.message()
        );
        let read_back = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            read_back.str_field("status"),
            "APPROVED",
            "case {case:?} ({target}): a refused transition writes nothing, got {:?}",
            read_back.body
        );
    }

    // And a record whose own asynchronous change has not finished is not in the workflow
    // yet. Only this service's own work puts a record in one of those statuses, so each
    // row is staged in its store directly.
    for current in ASYNCHRONOUS_STATUSES {
        {
            let state = engine
                .state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
            let mut state = state.lock();
            let record = state
                .registries
                .get_mut(&registry_id)
                .and_then(|registry| registry.records.get_mut(&record_id))
                .expect("the record the suite created");
            record.status = current.to_string();
        }
        let refused = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": "APPROVED", "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (409, Some("ConflictException")),
            "case \"a record in {current}\": expected the modelled 409 ConflictException, \
             got {} {:?}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(current),
            "case \"a record in {current}\": the refusal names the status the record is in, \
             got {:?}",
            refused.message()
        );
    }
}

#[tokio::test]
async fn a_registry_that_is_not_ready_has_no_curation_workflow() {
    let (router, engine) = router();
    let case = "a registry that is not READY";
    let (registry_id, _registry_arn, record_id, _record_arn) =
        record_in_status(&router, case, true, "APPROVED").await;

    // A registry is usable once it reaches READY. The statuses it can be in instead are
    // its own asynchronous ones (`RegistryStatus`: CREATE_FAILED, CREATING, DELETE_FAILED,
    // DELETING, READY, UPDATE_FAILED, UPDATING), which only this service's own work
    // reaches — so each row is staged in its store directly.
    for status in ["CREATING", "DELETING", "CREATE_FAILED", "DELETE_FAILED"] {
        {
            let state = engine
                .state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
            let mut state = state.lock();
            let registry = state
                .registries
                .get_mut(&registry_id)
                .expect("the registry the suite created");
            registry.status = status.to_string();
            // Nothing else may move it back while the row runs.
            registry.pending = None;
        }
        let refused = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": "DEPRECATED", "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (409, Some("ConflictException")),
            "case {case:?} ({status}): expected the modelled 409 ConflictException, got {} \
             {:?}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(status) && refused.message().contains("READY"),
            "case {case:?} ({status}): the refusal names the status the registry is in and \
             the one it has to be in, got {:?}",
            refused.message()
        );
    }
}

#[tokio::test]
async fn a_resource_this_service_does_not_hold_is_not_found() {
    let (router, _engine) = router();
    let case = "an absent resource";
    let (registry_id, _registry_arn, record_id, _record_arn) =
        record_in_status(&router, case, true, "APPROVED").await;

    // A registry id and a record id of the modelled shapes
    // (`RegistryId` ^[a-zA-Z0-9]{12,16}$, `RegistryRecordId` ^[a-zA-Z0-9]{12}$) that this
    // service holds nothing under, each given bare and as the resource's full ARN. The
    // sentences are the ones every other operation of this service states for these two
    // classes, with the identifier taken from the request.
    let absent_registry = "zzz999999999";
    let absent_record = "zzz999999999";
    let absent_registry_arn =
        format!("arn:aws:agent-registry:{REGION}:000000000000:registry/{absent_registry}");
    let absent_record_arn = format!(
        "arn:aws:agent-registry:{REGION}:000000000000:registry/{registry_id}/record/{absent_record}"
    );

    let rows: [(&str, &str, &str, String); 4] = [
        (
            "a registry id this service holds nothing under",
            absent_registry,
            record_id.as_str(),
            control::registry_not_found_message(absent_registry),
        ),
        (
            "the same registry named by its ARN",
            absent_registry_arn.as_str(),
            record_id.as_str(),
            control::registry_not_found_message(&absent_registry_arn),
        ),
        (
            "a record id this registry holds nothing under",
            registry_id.as_str(),
            absent_record,
            control::record_not_found_message(absent_record),
        ),
        (
            "the same record named by its ARN",
            registry_id.as_str(),
            absent_record_arn.as_str(),
            control::record_not_found_message(absent_record),
        ),
    ];

    for (row, registry, record, expected) in rows {
        let refused = update_status(
            &router,
            registry,
            record,
            json!({ "status": "APPROVED", "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (404, Some("ResourceNotFoundException"), expected.clone()),
            "case {row:?}: expected the modelled 404 with this service's own sentence \
             ({expected:?}), got {} {:?}",
            refused.status,
            refused.body
        );
    }
}
