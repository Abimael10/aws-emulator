// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateRegistryRecordStatus`, driven over HTTP through the real
//! router so every assertion goes through routing, deserialization, the handler and
//! serialization.
//!
//! The rule under test: `UpdateRegistryRecordStatus` is a step of the registry's curation
//! workflow — "Updates the status of a registry record as part of the registry's curation
//! workflow, for example to approve or reject a record that is pending approval, or to
//! deprecate an approved record so that it is no longer discoverable" — so the record's
//! stored status, not the caller, decides which status may follow it:
//!
//! * a caller may name only a status the curation workflow is its own: a status the SERVICE
//!   reaches by itself (`PENDING_APPROVAL`, entered by a submission, and the asynchronous
//!   `CREATING`, `UPDATING`, `CREATE_FAILED`, `UPDATE_FAILED`) is refused as a target;
//! * a record in the terminal `DEPRECATED` status is retired, so no later change applies;
//! * any other pair is taken only when the record lifecycle's transition table holds it,
//!   and refused with the statuses the stored status does allow otherwise;
//! * every refusal is the modelled `ValidationException` (HTTP 400) and writes nothing,
//!   while an allowed step answers 202, echoes the resulting status and the reason it was
//!   given, and persists both for every later read.
//!
//! A record whose own asynchronous change is still in flight, and a registry that has not
//! reached `READY`, are the modelled `ConflictException` (409): neither resource is in a
//! state the workflow runs in. A registry or a record this service does not hold is the
//! modelled `ResourceNotFoundException`, worded as every other operation of this service
//! words that class.
//!
//! Every test asserts over a TABLE of inputs, so it holds for a class of requests rather
//! than for one recorded shape: the rows span the status a record starts in, the status it
//! is moved to, the reason text carried with it, the identifier form each label is given
//! in (bare id and full ARN), and which of the two resources is absent.
//!
//! The rule is also an INVARIANT over a sequence of calls — a record reaches a status only
//! through steps the lifecycle holds, and a refused step writes nothing — so the last suite
//! here walks drawn sequences of curation steps and submissions over one record and checks
//! every answer against the status the PREVIOUS steps left, from seeds it owns.

use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
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

/// The statuses the record lifecycle allows a record in each status to move to, in the
/// order the service states them in the refusal of a transition it does not hold — written
/// out here rather than read from the crate, so the set and its order are pinned by the
/// test. Source: the transition table the service states for a status, whose `DRAFT` row AWS
/// states as `PENDING_APPROVAL, DEPRECATED, DRAFT, UPDATING, UPDATE_PENDING_AUTH`.
const VALID_TRANSITIONS: [(&str, &[&str]); 5] = [
    (
        "DRAFT",
        &[
            "PENDING_APPROVAL",
            "DEPRECATED",
            "DRAFT",
            "UPDATING",
            "UPDATE_PENDING_AUTH",
        ],
    ),
    (
        "PENDING_APPROVAL",
        &[
            "APPROVED",
            "REJECTED",
            "DEPRECATED",
            "PENDING_APPROVAL",
            "UPDATING",
            "UPDATE_PENDING_AUTH",
        ],
    ),
    (
        "APPROVED",
        &[
            "REJECTED",
            "DEPRECATED",
            "APPROVED",
            "UPDATING",
            "UPDATE_PENDING_AUTH",
        ],
    ),
    (
        "REJECTED",
        &[
            "DRAFT",
            "DEPRECATED",
            "REJECTED",
            "UPDATING",
            "UPDATE_PENDING_AUTH",
        ],
    ),
    ("CREATING", &["DRAFT", "CREATE_FAILED"]),
];

/// The statuses [`VALID_TRANSITIONS`] lists for `current`, as the set a step a caller asks
/// for is checked against.
fn valid_targets(current: &str) -> &'static [&'static str] {
    VALID_TRANSITIONS
        .iter()
        .find(|(status, _)| *status == current)
        .map(|(_, valid)| *valid)
        .unwrap_or_default()
}

/// The sentence this service refuses a transition its lifecycle does not hold with, built
/// from [`VALID_TRANSITIONS`] so the test states the expected text itself.
fn expected_transition_refusal(current: &str, target: &str) -> String {
    let valid = valid_targets(current).join(", ");
    format!(
        "Invalid status transition from {current} to {target}. \
         Valid transitions from {current}: {valid}"
    )
}

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

/// The reason the staging path leaves on a record it had to move through the workflow to
/// reach `start_status`, so a test can assert that a refused call changed nothing.
const STAGED_REASON: &str = "staged by the suite";

/// A `READY` registry holding one record in `start_status`, created through this service's
/// own operations and taken to that status through them: a submission enters the workflow
/// (auto-approved when the start status is past review), and the curation steps the
/// lifecycle allows carry it the rest of the way. Returns
/// (registryId, registryArn, recordId, recordArn).
async fn record_in_status(
    router: &MultiServiceRouter,
    case: &str,
    start_status: &str,
) -> (String, String, String, String) {
    // A submission is auto-approved where the start status is one past review, so the
    // record gets there without a curation step that the status table would refuse.
    let auto_approve = matches!(start_status, "APPROVED" | "DEPRECATED");
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
        let submitted_status = if auto_approve {
            "APPROVED"
        } else {
            "PENDING_APPROVAL"
        };
        record_reaching(router, case, &registry_id, &record_id, submitted_status).await;

        // The remaining statuses are curation steps the lifecycle allows from where the
        // submission left the record: PENDING_APPROVAL -> REJECTED, APPROVED -> DEPRECATED.
        if start_status != submitted_status {
            let moved = update_status(
                router,
                &registry_id,
                &record_id,
                json!({ "status": start_status, "statusReason": STAGED_REASON }),
            )
            .await;
            assert_eq!(
                (moved.status, moved.str_field("status")),
                (202, start_status),
                "case {case:?}: staging the record in {start_status} must succeed, got {} {:?}",
                moved.status,
                moved.body
            );
        }
    }
    (registry_id, registry_arn, record_id, record_arn)
}

#[tokio::test]
async fn a_curation_step_the_lifecycle_allows_is_written_with_its_reason_and_read_back() {
    let (router, _engine) = router();

    // (case, the status the record starts in, the status a curator moves it to, the reason
    // given). The rows span every transition the lifecycle's table allows a caller to ask
    // for — the workflow steps the operation's own doc names (approve or reject a record
    // pending approval, deprecate an approved one), approval withdrawn from an approved
    // record, retirement from any live status, a rejected record returning to draft, and a
    // repeat of the status a record already has (every row that does not end in the terminal
    // status repeats its step through the ARN form below) — and they differ in the reason
    // text so no fixed string can satisfy them.
    let rows: [(&str, &str, &str, &str); 10] = [
        (
            "approve a record pending approval",
            "PENDING_APPROVAL",
            "APPROVED",
            "reviewed by the catalogue team",
        ),
        (
            "reject a record pending approval",
            "PENDING_APPROVAL",
            "REJECTED",
            "descriptor does not describe a reachable endpoint",
        ),
        (
            "retire a record pending approval",
            "PENDING_APPROVAL",
            "DEPRECATED",
            "withdrawn before review",
        ),
        (
            "deprecate an approved record",
            "APPROVED",
            "DEPRECATED",
            "superseded by v2",
        ),
        (
            "re-approve an approved record",
            "APPROVED",
            "APPROVED",
            "re-reviewed after the owner's note",
        ),
        (
            "turn down a record that was approved",
            "APPROVED",
            "REJECTED",
            "approval withdrawn after a second look",
        ),
        (
            "retire a draft record",
            "DRAFT",
            "DEPRECATED",
            "never completed",
        ),
        ("keep a draft record in draft", "DRAFT", "DRAFT", ""),
        (
            "return a rejected record to draft",
            "REJECTED",
            "DRAFT",
            "owner is revising the descriptor",
        ),
        (
            "retire a rejected record",
            "REJECTED",
            "DEPRECATED",
            "abandoned after rejection",
        ),
    ];

    for (case, start_status, target, reason) in rows {
        let (registry_id, registry_arn, record_id, record_arn) =
            record_in_status(&router, case, start_status).await;

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
        // Repeating a status a record already holds is a step the table allows, except out
        // of the terminal one, which the terminal rule covers on its own.
        if target != "DEPRECATED" {
            let again = update_status(
                &router,
                &registry_arn,
                &record_arn,
                json!({ "status": target, "statusReason": "restated by the suite" }),
            )
            .await;
            assert_eq!(
                (
                    again.status,
                    again.str_field("status"),
                    again.str_field("statusReason")
                ),
                (202, target, "restated by the suite"),
                "case {case:?}: the ARN form of each label names the same resources, got {} \
                 {:?}",
                again.status,
                again.body
            );
        }
    }
}

#[tokio::test]
async fn a_status_the_service_reaches_by_itself_is_not_one_a_caller_can_name() {
    let (router, _engine) = router();

    // (the status the record starts in, the target a caller names). The targets span every
    // `RegistryRecordStatus` outside the curation ones: `PENDING_APPROVAL`, which only a
    // submission enters, and the four that report this service's own asynchronous work. The
    // rows start from two different statuses, and `PENDING_APPROVAL` is a status the DRAFT
    // row's own transition list holds — so it is refused as a TARGET a caller may not name,
    // not as a transition the table lacks.
    let mut rows: Vec<(&str, &str)> = vec![("DRAFT", "PENDING_APPROVAL")];
    for target in ASYNCHRONOUS_STATUSES {
        rows.push(("DRAFT", target));
    }
    rows.push(("APPROVED", "PENDING_APPROVAL"));
    rows.push(("APPROVED", "CREATING"));

    for (start_status, target) in rows {
        let case = format!("name {target} on a {start_status} record");
        let (registry_id, _registry_arn, record_id, _record_arn) =
            record_in_status(&router, &case, start_status).await;
        let before = get_record(&router, &registry_id, &record_id).await;

        let refused = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": target, "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (
                400,
                Some("ValidationException"),
                format!("Invalid target status: {target}")
            ),
            "case {case:?}: expected the modelled 400 ValidationException naming the target \
             it will not accept, got {} {:?}",
            refused.status,
            refused.body
        );

        // A refused call writes nothing: status and reason are what they were.
        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (after.str_field("status"), after.str_field("statusReason")),
            (before.str_field("status"), before.str_field("statusReason")),
            "case {case:?}: a refused transition writes nothing, got {:?}",
            after.body
        );
    }
}

#[tokio::test]
async fn a_transition_the_lifecycle_does_not_allow_is_refused_with_the_ones_it_allows() {
    let (router, _engine) = router();

    // (the status the record starts in, the target a curator asks for). The rows span the
    // pairs of curation statuses the lifecycle's table does NOT hold: a record skipping
    // review in either direction, a record pending approval or an approved one dragged back
    // to draft, and a rejected one approved without a new submission.
    let rows: [(&str, &str); 5] = [
        ("DRAFT", "APPROVED"),
        ("DRAFT", "REJECTED"),
        ("PENDING_APPROVAL", "DRAFT"),
        ("APPROVED", "DRAFT"),
        ("REJECTED", "APPROVED"),
    ];

    for (start_status, target) in rows {
        let case = format!("{start_status} to {target}");
        let (registry_id, _registry_arn, record_id, _record_arn) =
            record_in_status(&router, &case, start_status).await;
        let before = get_record(&router, &registry_id, &record_id).await;

        let refused = update_status(
            &router,
            &registry_id,
            &record_id,
            json!({ "status": target, "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (
                400,
                Some("ValidationException"),
                expected_transition_refusal(start_status, target)
            ),
            "case {case:?}: expected the modelled 400 ValidationException naming the \
             transitions {start_status} does allow, got {} {:?}",
            refused.status,
            refused.body
        );

        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (after.str_field("status"), after.str_field("statusReason")),
            (before.str_field("status"), before.str_field("statusReason")),
            "case {case:?}: a refused transition writes nothing, got {:?}",
            after.body
        );
    }
}

#[tokio::test]
async fn a_deprecated_record_is_retired_so_nothing_moves_it_again() {
    let (router, _engine) = router();
    let case = "a deprecated record";
    let (registry_id, registry_arn, record_id, record_arn) =
        record_in_status(&router, case, "DEPRECATED").await;

    // Every curation target a caller may name, asked for on a retired record — including
    // the status it already holds — and each identifier form of the two labels.
    let rows: [(&str, bool); 5] = [
        ("DRAFT", false),
        ("APPROVED", false),
        ("REJECTED", false),
        ("DEPRECATED", false),
        ("APPROVED", true),
    ];

    for (target, by_arn) in rows {
        let row = format!("{case}: {target}{}", if by_arn { " by ARN" } else { "" });
        let (registry, record) = if by_arn {
            (registry_arn.as_str(), record_arn.as_str())
        } else {
            (registry_id.as_str(), record_id.as_str())
        };
        let refused = update_status(
            &router,
            registry,
            record,
            json!({ "status": target, "statusReason": "asked for by the suite" }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (
                400,
                Some("ValidationException"),
                "Cannot update registry record in DEPRECATED status (terminal state)".to_string()
            ),
            "case {row:?}: expected the modelled 400 ValidationException stating the record \
             is terminal, got {} {:?}",
            refused.status,
            refused.body
        );

        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (after.str_field("status"), after.str_field("statusReason")),
            ("DEPRECATED", STAGED_REASON),
            "case {row:?}: the retired record keeps its status and its reason, got {:?}",
            after.body
        );
    }
}

/// A change asked of a record through one of the operations that CHANGE it: the operation's
/// name as the test reports it, and the request it sends for `registry`/`record`.
async fn change_asked_of(
    router: &MultiServiceRouter,
    operation: &str,
    registry: &str,
    record: &str,
) -> Reply {
    let records_path = format!(
        "/registries/{}/records/{}",
        encode_label(registry),
        encode_label(record)
    );
    match operation {
        "UpdateRegistryRecordStatus" => {
            update_status(
                router,
                registry,
                record,
                json!({ "status": "APPROVED", "statusReason": "asked for by the suite" }),
            )
            .await
        }
        "UpdateRegistryRecord" => {
            call(
                router,
                "PATCH",
                &records_path,
                Some(json!({ "description": { "optionalValue": "edited by the suite" } })),
            )
            .await
        }
        "SubmitRegistryRecordForApproval" => {
            call(
                router,
                "POST",
                &format!("{records_path}/submit-for-approval"),
                None,
            )
            .await
        }
        other => panic!("the suite asks for no operation {other}"),
    }
}

#[tokio::test]
async fn every_change_asked_of_a_retired_record_is_refused_with_the_one_terminal_sentence() {
    let (router, _engine) = router();
    let case = "a retired record";
    let (registry_id, registry_arn, record_id, record_arn) =
        record_in_status(&router, case, "DEPRECATED").await;

    // The sentence this service refuses a change to a retired record with: the record is past
    // the lifecycle, so the only variable part is the status it is stored in.
    let expected = "Cannot update registry record in DEPRECATED status (terminal state)";

    // Rows: every operation that CHANGES a stored record — a status change, an edit of its
    // fields and a submission into the approval workflow — each asked in both identifier forms
    // the two labels accept. WHICH change was asked for cannot change the answer: a retired
    // record admits none of them.
    for operation in [
        "UpdateRegistryRecordStatus",
        "UpdateRegistryRecord",
        "SubmitRegistryRecordForApproval",
    ] {
        for by_arn in [false, true] {
            let row = format!("{case}: {operation}{}", if by_arn { " by ARN" } else { "" });
            let (registry, record) = if by_arn {
                (registry_arn.as_str(), record_arn.as_str())
            } else {
                (registry_id.as_str(), record_id.as_str())
            };
            let refused = change_asked_of(&router, operation, registry, record).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code.as_deref(),
                    refused.message()
                ),
                (400, Some("ValidationException"), expected.to_string()),
                "case {row:?}: expected the modelled 400 ValidationException stating the \
                 record is terminal, got {} {:?}",
                refused.status,
                refused.body
            );

            let after = get_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                (after.str_field("status"), after.str_field("statusReason")),
                ("DEPRECATED", STAGED_REASON),
                "case {row:?}: the refused change writes nothing, got {:?}",
                after.body
            );
        }
    }
}

#[tokio::test]
async fn a_status_a_record_can_still_leave_is_refused_in_its_own_operations_words() {
    let (router, _engine) = router();

    // The counter-rows to the test above: a record in a status it can still LEAVE is refused
    // by each operation's own rule — the submission names the status a submission cannot be
    // made from, the status change names the transitions that status does allow — so the
    // terminal sentence is reserved for a record that is past the lifecycle.
    for (case, status, operation, expected) in [
        (
            "a record waiting for a curator",
            "PENDING_APPROVAL",
            "SubmitRegistryRecordForApproval",
            "Cannot submit the record for approval in current status: PENDING_APPROVAL"
                .to_string(),
        ),
        (
            "a record its registry approved on submission",
            "APPROVED",
            "SubmitRegistryRecordForApproval",
            "Cannot submit the record for approval in current status: APPROVED".to_string(),
        ),
        (
            "a record a curator turned down",
            "REJECTED",
            "UpdateRegistryRecordStatus",
            expected_transition_refusal("REJECTED", "APPROVED"),
        ),
    ] {
        let (registry_id, _registry_arn, record_id, _record_arn) =
            record_in_status(&router, case, status).await;
        let refused = change_asked_of(&router, operation, &registry_id, &record_id).await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (400, Some("ValidationException"), expected.clone()),
            "case {case:?} / {operation}: expected the modelled 400 ValidationException with \
             {expected:?}, got {} {:?}",
            refused.status,
            refused.body
        );
    }
}

#[tokio::test]
async fn a_record_whose_asynchronous_change_is_in_flight_is_not_in_the_workflow() {
    let (router, engine) = router();
    let case = "an asynchronous status";
    let (registry_id, _registry_arn, record_id, _record_arn) =
        record_in_status(&router, case, "APPROVED").await;

    // A record whose own asynchronous change has not finished is not in the workflow yet.
    // Only this service's own work puts a record in one of those statuses, so each row is
    // staged in its store directly.
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
        record_in_status(&router, case, "APPROVED").await;

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
        record_in_status(&router, case, "APPROVED").await;

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

/// Every member of `RegistryRecordStatus`, so a drawn step can name any status the model
/// admits as this operation's `status`. Source: that member's own enum (APPROVED |
/// CREATE_FAILED | CREATING | DEPRECATED | DRAFT | PENDING_APPROVAL | REJECTED |
/// UPDATE_FAILED | UPDATING), the catalogue
/// `crate::ops::types::REGISTRY_RECORD_STATUS_VALUES` holds.
const RECORD_STATUSES: [&str; 9] = [
    "APPROVED",
    "CREATE_FAILED",
    "CREATING",
    "DEPRECATED",
    "DRAFT",
    "PENDING_APPROVAL",
    "REJECTED",
    "UPDATE_FAILED",
    "UPDATING",
];

/// The statuses a caller may name as the TARGET of a curation step. Every other member of
/// the enum is one the SERVICE reaches by itself — `PENDING_APPROVAL` by a submission, and
/// the four asynchronous ones by this service's own create and update work.
const CURATION_TARGETS: [&str; 4] = ["DRAFT", "APPROVED", "REJECTED", "DEPRECATED"];

/// The record lifecycle's terminal status: a retired record admits no later change.
const TERMINAL_STATUS: &str = "DEPRECATED";

/// The status a submission a registry does not auto-approve settles the record at.
const SUBMITTED_STATUS: &str = "PENDING_APPROVAL";

/// The status a record is created in and returns to, and the only one a submission is made
/// from.
const DRAFT_STATUS: &str = "DRAFT";

/// The sentence this service refuses EVERY change to a retired record with, written out
/// here so the test states the expected text itself.
fn expected_terminal_refusal(status: &str) -> String {
    format!("Cannot update registry record in {status} status (terminal state)")
}

/// The sentence a submission is refused with when the record is in a status a submission
/// cannot be made from but can still leave.
fn expected_submission_refusal(status: &str) -> String {
    format!("Cannot submit the record for approval in current status: {status}")
}

/// One drawn step of the curation workflow: a status change naming a target, or a
/// submission for approval.
#[derive(Debug, Clone, Copy)]
enum Step {
    Status(&'static str),
    Submit,
}

/// What the record lifecycle answers `step` with for a record STORED in `current`: the
/// status the record settles at, or the sentence the service refuses the step with.
///
/// Computed from the stored status alone and in the order the service states its rules — a
/// target no caller may name, then the terminal status, then the transition table for a
/// status change and the submission's own precondition for a submission — so the expectation
/// is the rule rather than a transcript of one sequence.
fn expected_outcome(current: &str, step: Step) -> Result<&'static str, String> {
    match step {
        Step::Status(target) => {
            if !CURATION_TARGETS.contains(&target) {
                Err(format!("Invalid target status: {target}"))
            } else if current == TERMINAL_STATUS {
                Err(expected_terminal_refusal(current))
            } else if valid_targets(current).contains(&target) {
                Ok(target)
            } else {
                Err(expected_transition_refusal(current, target))
            }
        }
        Step::Submit => {
            if current == TERMINAL_STATUS {
                Err(expected_terminal_refusal(current))
            } else if current == DRAFT_STATUS {
                Ok(SUBMITTED_STATUS)
            } else {
                Err(expected_submission_refusal(current))
            }
        }
    }
}

/// The seeds the drawn walk below runs from: a fixed set the suite owns, so the case count
/// is bounded and every run walks the same sequences, or the single seed
/// [`WALK_SEED_OVERRIDE`] names for an exploratory run. Every assertion carries its seed and
/// its step index, so a failure names the walk that produced it and `WALK_SEED_OVERRIDE`
/// replays exactly that one.
const WALK_SEEDS: [u64; 4] = [0x5eed_0001, 0x5eed_0002, 0x5eed_0003, 0x5eed_0004];

/// The environment variable that replaces [`WALK_SEEDS`] with one named seed.
const WALK_SEED_OVERRIDE: &str = "AGENT_REGISTRY_CURATION_WALK_SEED";

/// How many steps one walk takes. Bounded, and long enough that a walk reaches the statuses
/// past review and the terminal one and keeps asking for changes afterwards.
const WALK_STEPS: usize = 14;

#[tokio::test]
async fn a_drawn_walk_of_the_workflow_reaches_only_a_status_the_lifecycle_allows() {
    let (router, _engine) = router();

    let seeds = match std::env::var(WALK_SEED_OVERRIDE) {
        Ok(named) => vec![named.trim().parse::<u64>().unwrap_or_else(|error| {
            panic!("{WALK_SEED_OVERRIDE}={named:?} is not a u64 seed: {error}")
        })],
        Err(_) => WALK_SEEDS.to_vec(),
    };

    for seed in seeds {
        let case = format!("walk from seed {seed}");
        let (registry_id, _registry_arn, record_id, _record_arn) =
            record_in_status(&router, &case, DRAFT_STATUS).await;
        let mut generator = StdRng::seed_from_u64(seed);

        // The status and the reason the record is STORED with, tracked beside the service so
        // every step is judged against what the steps before it left rather than against the
        // request that was just sent.
        let mut current = DRAFT_STATUS;
        let mut reason = String::new();

        for index in 0..WALK_STEPS {
            // One extra draw past the status catalogue stands for a submission, so a walk
            // mixes the two ways a record moves through the workflow.
            let drawn = generator.random_range(0..=RECORD_STATUSES.len());
            let step = if drawn == RECORD_STATUSES.len() {
                Step::Submit
            } else {
                Step::Status(RECORD_STATUSES[drawn])
            };
            let row = format!("{case}, step {index}: {step:?} on a {current} record");
            let given = format!("drawn step {index} of seed {seed}");

            let reply = match step {
                Step::Status(target) => {
                    update_status(
                        &router,
                        &registry_id,
                        &record_id,
                        json!({ "status": target, "statusReason": given.clone() }),
                    )
                    .await
                }
                Step::Submit => {
                    call(
                        &router,
                        "POST",
                        &format!(
                            "/registries/{}/records/{}/submit-for-approval",
                            encode_label(&registry_id),
                            encode_label(&record_id)
                        ),
                        None,
                    )
                    .await
                }
            };

            match expected_outcome(current, step) {
                Ok(settled) => {
                    assert_eq!(
                        (reply.status, reply.str_field("status")),
                        (202, settled),
                        "case {row:?}: a step the lifecycle allows answers 202 with the status \
                         it settles the record at, got {} {:?}",
                        reply.status,
                        reply.body
                    );
                    current = settled;
                    // A status change carries the reason it was given; a submission decides
                    // nothing about the status it enters, so it leaves no reason behind.
                    reason = match step {
                        Step::Status(_) => given,
                        Step::Submit => String::new(),
                    };
                }
                Err(expected) => {
                    assert_eq!(
                        (
                            reply.status,
                            reply.error_code.as_deref(),
                            reply.message()
                        ),
                        (400, Some("ValidationException"), expected.clone()),
                        "case {row:?}: expected the modelled 400 ValidationException with \
                         {expected:?}, got {} {:?}",
                        reply.status,
                        reply.body
                    );
                }
            }

            // What the walk left is what a later read serves: a step that was taken is
            // stored with its reason, and a step that was refused wrote nothing at all.
            let read_back = get_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                (
                    read_back.status,
                    read_back.str_field("status"),
                    read_back.str_field("statusReason")
                ),
                (200, current, reason.as_str()),
                "case {row:?}: the stored record must carry the status and the reason the \
                 walk left it with, got {} {:?}",
                read_back.status,
                read_back.body
            );
        }

        // Every walk ends the same way, wherever it was left: retirement is a step the table
        // allows out of every status a walk can stand in, and a retired record is past the
        // lifecycle but still one its owner can remove.
        if current != TERMINAL_STATUS {
            let retired = update_status(
                &router,
                &registry_id,
                &record_id,
                json!({
                    "status": TERMINAL_STATUS,
                    "statusReason": "retired at the end of the walk",
                }),
            )
            .await;
            assert_eq!(
                (retired.status, retired.str_field("status")),
                (202, TERMINAL_STATUS),
                "case {case:?}: retiring a {current} record is a step the lifecycle allows, \
                 got {} {:?}",
                retired.status,
                retired.body
            );
        }

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
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteRegistryRecord removes a retired record, got {} {:?}",
            deleted.status, deleted.body
        );
        let gone = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (gone.status, gone.error_code.as_deref(), gone.message()),
            (
                404,
                Some("ResourceNotFoundException"),
                control::record_not_found_message(&record_id)
            ),
            "case {case:?}: the removed record is gone from every later read, got {} {:?}",
            gone.status,
            gone.body
        );
    }
}
