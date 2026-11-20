// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `SubmitRegistryRecordForApproval`, driven over HTTP through the
//! real router so every assertion goes through routing, deserialization, the handler and
//! serialization.
//!
//! The rule under test: submitting a `DRAFT` record moves it into the approval workflow
//! of the registry that holds it, and the status it lands in is computed from that
//! registry's approval configuration as it stands at submission time — a configuration
//! carrying an auto-approval rule that covers the record approves it on submission,
//! anything else (no configuration, an empty rule list) leaves it waiting for manual
//! review. The resulting status is persisted, so every later read reports it, and a
//! record in any other status cannot be submitted at all.
//!
//! Two rules about the ANSWER go with it:
//!
//! * the instant the answer reports is a timestamp in the form its target timestamp shape
//!   declares — `DateTimestamp` carries `@timestampFormat("date-time")`, so it is an
//!   RFC 3339 / ISO 8601 date-time STRING in UTC and never an epoch number;
//! * a record whose stored status is not `DRAFT` is a precondition on the resource's
//!   state, so the submission is the modelled `ValidationException` (HTTP 400): a status
//!   the record can still leave is worded in this operation's own one sentence, whose only
//!   variable part is that stored status, while the lifecycle's TERMINAL status is answered
//!   with the one sentence every change to a retired record is refused with.
//!
//! Every test asserts over a TABLE of inputs, so it holds for a class of requests rather
//! than for one recorded shape.

use std::sync::Arc;
use std::time::Duration;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region in the test's SigV4 credential scope; every minted ARN must carry it.
const REGION: &str = "us-west-2";

/// A signed-looking request, so the router resolves this service's signing name
/// (`agent-registry`) and the handlers read the region from the credential scope.
/// The signature itself is never checked: the test router runs with authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// How long a wait for a transition the clock owes (a registry becoming `READY`, a record
/// becoming `DRAFT`) may take before a test gives up. Generous on purpose: the wait is on
/// the CONDITION, so a loaded host only polls more times.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll loop pauses between two looks at a resource.
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

/// A sortable key for a modelled timestamp, whichever wire form the protocol renders it
/// in: epoch seconds (zero-padded so the keys sort numerically) or an RFC-3339 string
/// (which sorts lexicographically). Empty for anything that is not a timestamp at all.
fn timestamp_key(value: &Value) -> String {
    match value {
        Value::Number(number) => format!("{:020.3}", number.as_f64().unwrap_or_default()),
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
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

async fn list_records(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records-list", encode_label(registry_id)),
        Some(body),
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

/// Create a registry with `body` and wait for the creation the clock owes to come due, so
/// it is `READY`. The wait is on the CONDITION, never on a fixed span. Returns its
/// registry id.
async fn ready_registry(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let created = create_registry(router, body).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry must answer 202, got {} {:?}",
        created.status, created.body
    );
    let registry_id = id_of(created.str_field("registryArn")).to_string();
    let started = std::time::Instant::now();
    loop {
        let got = get_registry(router, &registry_id).await;
        if got.str_field("status") == "READY" {
            return registry_id;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: a created registry becomes READY, last answer {} {:?}",
            got.status,
            got.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at a record until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes, or a status another call has written —
/// never on a fixed span.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) {
    let started = std::time::Instant::now();
    loop {
        let got = get_record(router, registry_id, record_id).await;
        if got.str_field("status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the record never reached {wanted}, last answer {} {:?}",
            got.status,
            got.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

/// Create a record of `record_type` named `name` and wait for the creation the clock owes
/// to come due, so it is `DRAFT`. Returns its record id.
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
        "case {case:?}: CreateRegistryRecord must answer 202, got {} {:?}",
        created.status, created.body
    );
    let record_id = id_of(created.str_field("recordArn")).to_string();
    record_reaching(router, case, registry_id, &record_id, "DRAFT").await;
    record_id
}

/// Move a record to another status through the registry's curation workflow, so a test can
/// address a record this service's own operations put in a status other than `DRAFT`.
async fn update_status(
    router: &MultiServiceRouter,
    registry_id: &str,
    record_id: &str,
    body: Value,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!(
            "/registries/{}/records/{}/status",
            encode_label(registry_id),
            encode_label(record_id)
        ),
        Some(body),
    )
    .await
}

/// The approval configurations a registry can carry, each with the status its registry
/// gives a submitted record. `autoApprovalRules` entries come from
/// `AutoApprovalRule`'s own catalogue (`smithy_query(query="shape",
/// shape="AutoApprovalRule")`), whose only value is `APPROVE_ALL`; the shape documents
/// that an omitted or empty rule list means manual review.
fn approval_configuration_table() -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "no approvalConfiguration at all",
            json!({}),
            "PENDING_APPROVAL",
        ),
        (
            "an approvalConfiguration with no rule list",
            json!({ "approvalConfiguration": {} }),
            "PENDING_APPROVAL",
        ),
        (
            "an empty autoApprovalRules list",
            json!({ "approvalConfiguration": { "autoApprovalRules": [] } }),
            "PENDING_APPROVAL",
        ),
        (
            "the APPROVE_ALL auto-approval rule",
            json!({ "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] } }),
            "APPROVED",
        ),
    ]
}

#[tokio::test]
async fn submitting_a_draft_record_lands_in_the_status_the_registrys_approval_configuration_decides()
{
    // Rows differ along the registry's approval configuration (the member that decides
    // the outcome) and along the record type submitted, so the answer cannot be a fixed
    // status for a fixed request shape.
    let record_types = ["MCP", "AGENT", "CUSTOM"];
    let router = router();

    for (index, (case, configuration, expected_status)) in
        approval_configuration_table().into_iter().enumerate()
    {
        let mut body = json!({ "name": format!("approval-registry-{index}") });
        for (member, value) in configuration.as_object().into_iter().flatten() {
            body[member] = value.clone();
        }
        let registry_id = ready_registry(&router, case, body).await;

        for record_type in record_types {
            let case = format!("{case} / {record_type}");
            let record_id = draft_record(
                &router,
                &case,
                &registry_id,
                &format!("record-{record_type}"),
                record_type,
            )
            .await;
            let before = get_record(&router, &registry_id, &record_id).await;

            let submitted = submit_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                submitted.status, 202,
                "case {case:?}: SubmitRegistryRecordForApproval answers 202, got {} {:?}",
                submitted.status, submitted.body
            );
            assert_eq!(
                submitted.str_field("status"),
                expected_status,
                "case {case:?}: expected status {expected_status}, got {:?}",
                submitted.body
            );
            assert_eq!(
                submitted.str_field("recordId"),
                record_id,
                "case {case:?}: expected recordId {record_id}, got {:?}",
                submitted.body
            );
            assert_eq!(
                submitted.str_field("recordArn"),
                before.str_field("recordArn"),
                "case {case:?}: expected recordArn {:?}, got {:?}",
                before.str_field("recordArn"),
                submitted.body
            );
            assert_eq!(
                submitted.str_field("registryArn"),
                before.str_field("registryArn"),
                "case {case:?}: expected registryArn {:?}, got {:?}",
                before.str_field("registryArn"),
                submitted.body
            );
            assert!(
                timestamp_key(&submitted.body["updatedAt"])
                    >= timestamp_key(&before.body["updatedAt"]),
                "case {case:?}: updatedAt must not go backwards from {:?}, got {:?}",
                before.body["updatedAt"],
                submitted.body["updatedAt"]
            );

            // The status is PERSISTED: a later read reports it, not the DRAFT the
            // record was in before.
            let after = get_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                after.str_field("status"),
                expected_status,
                "case {case:?}: GetRegistryRecord reports the submitted status \
                 {expected_status}, got {:?}",
                after.body
            );
            assert_eq!(
                timestamp_key(&after.body["updatedAt"]),
                timestamp_key(&submitted.body["updatedAt"]),
                "case {case:?}: GetRegistryRecord must report the updatedAt the submit \
                 answered ({:?}), got {:?}",
                submitted.body["updatedAt"],
                after.body["updatedAt"]
            );
            assert_eq!(
                timestamp_key(&after.body["createdAt"]),
                timestamp_key(&before.body["createdAt"]),
                "case {case:?}: createdAt is not touched by a submission; expected {:?}, \
                 got {:?}",
                before.body["createdAt"],
                after.body["createdAt"]
            );
        }
    }
}

#[tokio::test]
async fn a_submitted_record_is_found_by_a_status_filter_and_hidden_from_the_others() {
    // Each row submits a record into a registry with its own approval configuration, so
    // the records in one registry end up spread across the statuses a submission can
    // produce plus the DRAFT of one never submitted.
    let router = router();
    let registry_id = ready_registry(
        &router,
        "status filter",
        json!({
            "name": "filterable-registry",
            "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] }
        }),
    )
    .await;
    let approved = draft_record(&router, "approved", &registry_id, "auto", "MCP").await;
    let still_draft = draft_record(&router, "draft", &registry_id, "untouched", "AGENT").await;
    let submitted = submit_record(&router, &registry_id, &approved).await;
    assert_eq!(
        submitted.str_field("status"),
        "APPROVED",
        "case \"status filter\": expected APPROVED, got {:?}",
        submitted.body
    );

    let manual_registry = ready_registry(
        &router,
        "manual review",
        json!({ "name": "manual-registry" }),
    )
    .await;
    let pending = draft_record(&router, "pending", &manual_registry, "manual", "SKILL").await;
    let submitted = submit_record(&router, &manual_registry, &pending).await;
    assert_eq!(
        submitted.str_field("status"),
        "PENDING_APPROVAL",
        "case \"manual review\": expected PENDING_APPROVAL, got {:?}",
        submitted.body
    );

    // Rows span every status a record in these registries can be in, including two the
    // model declares that nothing here reached.
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("APPROVED", "APPROVED", vec![approved.as_str()]),
        ("DRAFT", "DRAFT", vec![still_draft.as_str()]),
        ("PENDING_APPROVAL", "PENDING_APPROVAL", vec![]),
        ("CREATING", "CREATING", vec![]),
        ("REJECTED", "REJECTED", vec![]),
    ];
    for (case, status, expected) in cases {
        let listed = list_records(
            &router,
            &registry_id,
            json!({ "filters": [{ "name": "status", "values": [status] }] }),
        )
        .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: ListRegistryRecords answers 200, got {} {:?}",
            listed.status, listed.body
        );
        let ids: Vec<&str> = listed.body["registryRecords"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .map(|record| record["recordId"].as_str().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            ids, expected,
            "case {case:?}: a status filter returns exactly the records in that status; \
             expected {expected:?}, got {ids:?}"
        );
    }

    // The record submitted in the OTHER registry is not reachable through this one, so a
    // submission writes to the registry that holds the record and to no other.
    let listed = list_records(
        &router,
        &registry_id,
        json!({ "filters": [{ "name": "status", "values": ["PENDING_APPROVAL"] }] }),
    )
    .await;
    assert_eq!(
        listed.body["registryRecords"].as_array().map(Vec::len),
        Some(0),
        "case \"cross-registry\": the other registry's PENDING_APPROVAL record must not \
         be listed here, got {:?}",
        listed.body
    );
}

#[tokio::test]
async fn a_record_that_is_not_draft_cannot_be_submitted() {
    // Rows differ along WHY the record is not submittable: it is still being created,
    // or it has already been through the submission once. Either way the refusal is the
    // one AWS answers for a record whose status admits no submission — a 400
    // `ValidationException` whose sentence names that status.
    let router = router();

    for (case, auto_approve) in [("manual review", false), ("auto-approved", true)] {
        let mut body = json!({ "name": format!("conflicting-{}", case.replace(' ', "-")) });
        if auto_approve {
            body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
        }
        let registry_id = ready_registry(&router, case, body).await;

        // Still CREATING: the creation the clock owes has not come due yet.
        let created = create_record(
            &router,
            &registry_id,
            json!({
                "name": "fresh",
                "recordType": "MCP",
                "descriptors": descriptors_for("MCP"),
            }),
        )
        .await;
        let fresh = id_of(created.str_field("recordArn")).to_string();
        let refused = submit_record(&router, &registry_id, &fresh).await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (
                400,
                Some("ValidationException"),
                "Cannot submit the record for approval in current status: CREATING".to_string()
            ),
            "case {case:?} / CREATING: a record that is not DRAFT is refused with the \
             modelled 400 naming its status, got {} {:?}",
            refused.status,
            refused.body
        );

        // Already submitted: the second submission is refused and the status the first
        // one produced is left alone.
        let record_id = draft_record(&router, case, &registry_id, "submitted-once", "AGENT").await;
        let first = submit_record(&router, &registry_id, &record_id).await;
        let settled = first.str_field("status").to_string();
        let again = submit_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (again.status, again.error_code.as_deref(), again.message()),
            (
                400,
                Some("ValidationException"),
                format!("Cannot submit the record for approval in current status: {settled}")
            ),
            "case {case:?} / resubmitted: a record in {settled} cannot be submitted again, \
             got {} {:?}",
            again.status,
            again.body
        );
        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            after.str_field("status"),
            settled,
            "case {case:?} / resubmitted: the refused resubmission must not change the \
             stored status {settled}, got {:?}",
            after.body
        );
    }
}

#[tokio::test]
async fn submitting_accepts_either_identifier_form_and_refuses_an_unknown_resource() {
    let router = router();
    let registry_id = ready_registry(
        &router,
        "identifier forms",
        json!({ "name": "identifier-registry" }),
    )
    .await;
    let registry = get_registry(&router, &registry_id).await;
    let registry_arn = registry.str_field("registryArn").to_string();

    // Both labels accept the resource's bare id or its full ARN, so the rows differ
    // along which form each label carries.
    let record_arn = {
        let record_id = draft_record(&router, "arn form", &registry_id, "arn-form", "MCP").await;
        get_record(&router, &registry_id, &record_id)
            .await
            .str_field("recordArn")
            .to_string()
    };
    let cases: Vec<(&str, String, String)> = vec![
        (
            "both as bare ids",
            registry_id.clone(),
            id_of(&record_arn).to_string(),
        ),
        ("registry as ARN", registry_arn.clone(), {
            draft_record(&router, "registry as ARN", &registry_id, "second", "AGENT").await
        }),
        ("record as ARN", registry_id.clone(), {
            let id = draft_record(&router, "record as ARN", &registry_id, "third", "SKILL").await;
            format!("{registry_arn}/record/{id}")
        }),
    ];
    for (case, registry_label, record_label) in cases {
        let submitted = submit_record(&router, &registry_label, &record_label).await;
        assert_eq!(
            submitted.status, 202,
            "case {case:?}: both identifier forms are accepted, got {} {:?}",
            submitted.status, submitted.body
        );
        assert_eq!(
            submitted.str_field("status"),
            "PENDING_APPROVAL",
            "case {case:?}: expected PENDING_APPROVAL, got {:?}",
            submitted.body
        );
        assert_eq!(
            submitted.str_field("registryArn"),
            registry_arn,
            "case {case:?}: expected registryArn {registry_arn}, got {:?}",
            submitted.body
        );
    }

    // A resource that does not exist is a 404, never a submission against nothing. The
    // ids below satisfy the labels' patterns (12 alphanumerics), so the handler, not the
    // boundary validator, is what refuses them.
    let known_record = id_of(&record_arn).to_string();
    let not_found: Vec<(&str, String, String)> = vec![
        (
            "unknown registry",
            "ffffffffffff".to_string(),
            known_record.clone(),
        ),
        (
            "unknown record",
            registry_id.clone(),
            "ffffffffffff".to_string(),
        ),
        (
            "a record of another registry",
            ready_registry(&router, "another registry", json!({ "name": "other-registry" })).await,
            known_record.clone(),
        ),
    ];
    for (case, registry_label, record_label) in not_found {
        let refused = submit_record(&router, &registry_label, &record_label).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
    }
}

#[tokio::test]
async fn a_record_held_by_a_registry_stays_submittable_because_that_registry_is_not_deleted() {
    // A registry is deleted only once it is empty, so a record never lives under a
    // registry that is being deleted: the delete is refused while the record is there,
    // which leaves the registry READY and its record submittable, and the registry can
    // only be deleted after the record has been — at which point a submission names a
    // record that is gone.
    let case = "a registry holding a record";
    let router = router();
    let registry_id = ready_registry(&router, case, json!({ "name": "doomed-registry" })).await;
    let record_id = draft_record(&router, case, &registry_id, "doomed", "MCP").await;
    let delete_registry_path = format!("/registries/{}", encode_label(&registry_id));

    let refused_delete = call(&router, "DELETE", &delete_registry_path, None).await;
    assert_eq!(
        refused_delete.status, 409,
        "case {case:?}: a registry holding records refuses the delete, got {} {:?}",
        refused_delete.status, refused_delete.body
    );

    let submitted = submit_record(&router, &registry_id, &record_id).await;
    assert_eq!(
        submitted.status, 202,
        "case {case:?}: the registry is untouched, so its record can still be submitted, \
         got {} {:?}",
        submitted.status, submitted.body
    );
    assert_eq!(
        submitted.str_field("status"),
        "PENDING_APPROVAL",
        "case {case:?}: the registry auto-approves nothing, got {:?}",
        submitted.body
    );

    let deleted_record = call(
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
        deleted_record.status, 200,
        "case {case:?}: DeleteRegistryRecord answers 200, got {} {:?}",
        deleted_record.status, deleted_record.body
    );
    let deleted = call(&router, "DELETE", &delete_registry_path, None).await;
    assert_eq!(
        deleted.status, 202,
        "case {case:?}: an emptied registry is deleted with 202, got {} {:?}",
        deleted.status, deleted.body
    );

    let refused = submit_record(&router, &registry_id, &record_id).await;
    assert_eq!(
        refused.status, 404,
        "case {case:?}: a record that is gone cannot be submitted, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        refused.error_code
    );
    assert!(
        refused.message().contains(&record_id),
        "case {case:?}: the message must name the record, got {:?}",
        refused.message()
    );
}

/// Whether `text` is the `date-time` form Smithy declares — an RFC 3339 instant in UTC,
/// `YYYY-MM-DDTHH:MM:SS[.fraction]Z`. Written out rather than delegated so the SHAPE of
/// the text is asserted and not merely that some parser accepted it.
fn is_utc_date_time_text(text: &str) -> bool {
    let Some(civil) = text.strip_suffix('Z') else {
        return false;
    };
    let Some((date, time)) = civil.split_once('T') else {
        return false;
    };
    let date_fields: Vec<&str> = date.split('-').collect();
    if date_fields.len() != 3 {
        return false;
    }
    for (field, width) in date_fields.iter().zip([4, 2, 2]) {
        if field.len() != width || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let clock_fields: Vec<&str> = clock.split(':').collect();
    if clock_fields.len() != 3 {
        return false;
    }
    for field in &clock_fields {
        if field.len() != 2 || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    match fraction {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        None => true,
    }
}

/// The instant a reported timestamp names, asserting the wire form on the way: the member
/// is a STRING in the UTC date-time form, a client's own date-time parser reads it, and
/// re-rendering what was read gives the same text back.
fn date_time_instant(case: &str, path: &str, value: &Value) -> f64 {
    let text = match value {
        Value::String(text) => text.clone(),
        other => {
            panic!("case {case:?}: {path} must be an RFC 3339 date-time string, got {other:?}")
        }
    };
    assert!(
        is_utc_date_time_text(&text),
        "case {case:?}: {path} must be YYYY-MM-DDTHH:MM:SS[.fraction]Z, got {text:?}"
    );
    let parsed = match DateTime::from_str(&text, Format::DateTime) {
        Ok(parsed) => parsed,
        Err(error) => {
            panic!("case {case:?}: {path} must parse as a date-time, got {text:?} ({error})")
        }
    };
    let rendered = parsed.fmt(Format::DateTime).ok();
    assert_eq!(
        rendered.as_deref(),
        Some(text.as_str()),
        "case {case:?}: {path} must survive a date-time round trip, got {rendered:?} from \
         {text:?}"
    );
    parsed.as_secs_f64()
}

#[tokio::test]
async fn the_answer_to_a_submission_reports_updated_at_as_a_date_time_string() {
    let router = router();

    // Rows differ along the registry's approval configuration (so the submitted record
    // lands in each status a submission can produce) and along the record's own type (so
    // each row carries a descriptor of a different shape): the WIRE FORM of the instant
    // cannot depend on either.
    let record_types = ["MCP", "AGENT", "SKILL", "GATEWAY"];
    for (index, (case, registry_body, expected_status)) in
        approval_configuration_table().into_iter().enumerate()
    {
        let mut registry_body = registry_body;
        registry_body["name"] = json!(format!("submit-date-time-{index}"));
        let record_type = record_types[index % record_types.len()];
        let registry_id = ready_registry(&router, case, registry_body).await;
        let record_id =
            draft_record(&router, case, &registry_id, "date.time.probe", record_type).await;

        let before = get_record(&router, &registry_id, &record_id).await;
        let created = date_time_instant(case, "$.createdAt of the record", &before.body["createdAt"]);

        let submitted = submit_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (submitted.status, submitted.str_field("status")),
            (202, expected_status),
            "case {case:?}: a submission answers 202 with the status the registry decides, \
             got {} {:?}",
            submitted.status,
            submitted.body
        );
        let submitted_at =
            date_time_instant(case, "$.updatedAt of the submission", &submitted.body["updatedAt"]);
        assert!(
            submitted_at >= created,
            "case {case:?}: the submission's updatedAt is not before the record was created, \
             got {submitted_at} against {created}"
        );

        // The instant the submission answered is the one every later read reports, in the
        // same form.
        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            after.body["updatedAt"], submitted.body["updatedAt"],
            "case {case:?}: GetRegistryRecord reports the instant the submission answered, \
             got {:?} against {:?}",
            after.body["updatedAt"], submitted.body["updatedAt"]
        );
        date_time_instant(case, "$.updatedAt of the read-back record", &after.body["updatedAt"]);
    }
}

/// A `READY` registry holding one record in `start_status`, reached through this service's
/// own operations: a submission enters the workflow (auto-approved where the registry's
/// configuration says so), and the curation steps the lifecycle allows carry the record the
/// rest of the way. Returns (registryId, recordId).
async fn record_in_status(
    router: &MultiServiceRouter,
    case: &str,
    index: usize,
    start_status: &str,
) -> (String, String) {
    // A submission is auto-approved where the start status is the one past review, so the
    // record gets there without a curation step the lifecycle would refuse.
    let mut registry_body = json!({ "name": format!("submit-refusal-{index}") });
    if start_status == "APPROVED" {
        registry_body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
    }
    let registry_id = ready_registry(router, case, registry_body).await;
    let record_id = draft_record(router, case, &registry_id, "refusal.probe", "CUSTOM").await;
    if start_status == "DRAFT" {
        return (registry_id, record_id);
    }

    // Retirement is reached from the record's own DRAFT status, so a retired record never
    // entered the approval workflow at all; every other status is past a submission.
    if start_status != "DEPRECATED" {
        let submitted = submit_record(router, &registry_id, &record_id).await;
        assert_eq!(
            submitted.status, 202,
            "case {case:?}: staging needs the submission to answer 202, got {} {:?}",
            submitted.status, submitted.body
        );
    }
    let entered = get_record(router, &registry_id, &record_id).await;
    if entered.str_field("status") != start_status {
        let moved = update_status(
            router,
            &registry_id,
            &record_id,
            json!({ "status": start_status, "statusReason": "staged by the suite" }),
        )
        .await;
        assert_eq!(
            moved.status, 202,
            "case {case:?}: staging the record in {start_status} must answer 202, got {} {:?}",
            moved.status, moved.body
        );
    }
    record_reaching(router, case, &registry_id, &record_id, start_status).await;
    (registry_id, record_id)
}

#[tokio::test]
async fn a_record_in_any_status_other_than_draft_is_refused_with_the_sentence_of_its_class() {
    let router = router();

    // Rows span the statuses other than DRAFT a stored record can be addressed in: the two
    // a submission itself produces, the one a curator's rejection produces, and the
    // lifecycle's terminal status. (`CREATING` — and with it the class of statuses this
    // service reaches only while its own asynchronous work is in flight — is the first row
    // of `a_record_that_is_not_draft_cannot_be_submitted` above.) The values come from
    // `RegistryRecordStatus`' own catalogue (`smithy_query(query="shape",
    // shape="RegistryRecordStatus")`: DRAFT, PENDING_APPROVAL, APPROVED, REJECTED,
    // DEPRECATED, CREATING, UPDATING, CREATE_FAILED, UPDATE_FAILED).
    //
    // A status a record can still LEAVE is refused in this operation's own words, naming that
    // status; the lifecycle's TERMINAL status is not about submission at all — the record is
    // retired, so every change to it is refused with the one sentence that states that, the
    // same sentence an edit and a status change answer with.
    let rows: [(&str, &str); 4] = [
        ("a record waiting for a curator", "PENDING_APPROVAL"),
        ("a record its registry approved on submission", "APPROVED"),
        ("a record a curator turned down", "REJECTED"),
        ("a retired record", "DEPRECATED"),
    ];

    for (index, (case, status)) in rows.into_iter().enumerate() {
        let (registry_id, record_id) = record_in_status(&router, case, index, status).await;
        let before = get_record(&router, &registry_id, &record_id).await;
        let expected = if status == "DEPRECATED" {
            format!("Cannot update registry record in {status} status (terminal state)")
        } else {
            format!("Cannot submit the record for approval in current status: {status}")
        };

        let refused = submit_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (400, Some("ValidationException"), expected.clone()),
            "case {case:?}: a record in {status} is refused with the modelled 400 and the \
             sentence of its class {expected:?}, got {} {:?}",
            refused.status,
            refused.body
        );

        // The refusal wrote nothing: the record is still where the workflow left it.
        let after = get_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (after.str_field("status"), &after.body["updatedAt"]),
            (status, &before.body["updatedAt"]),
            "case {case:?}: a refused submission changes nothing, got {:?}",
            after.body
        );
    }
}
