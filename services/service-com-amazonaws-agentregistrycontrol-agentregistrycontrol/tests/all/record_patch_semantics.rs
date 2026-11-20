// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateRegistryRecord`, driven over HTTP through the real router
//! so every assertion goes through routing, deserialization, the handler and
//! serialization.
//!
//! The rule under test: an update is a PATCH of the STORED record — "Fields that use
//! update wrappers follow PATCH semantics: omit the field to leave it unchanged", a
//! wrapper carrying a value replaces the stored value and a wrapper supplied empty unsets
//! the field it wraps — and the update is asynchronous, so the record carries `UPDATING`
//! while it is processed and then settles. What was written is what every later read
//! reports, through `GetRegistryRecord` and through the `ListRegistryRecords` summary
//! alike, since both project the one stored record.
//!
//! Every test asserts over a TABLE of inputs, so it holds for a class of requests rather
//! than for one recorded shape: the rows span which member is patched, whether it is set
//! or unset, how deep in the descriptor tree it sits, the type of the value (string,
//! document, nested structure), the number of members patched at once, and the status the
//! record is in when the edit arrives.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
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

async fn update_record(
    router: &MultiServiceRouter,
    registry: &str,
    record: &str,
    body: Value,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!(
            "/registries/{}/records/{}",
            encode_label(registry),
            encode_label(record)
        ),
        Some(body),
    )
    .await
}

async fn list_records(router: &MultiServiceRouter, registry: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{}/records-list", encode_label(registry)),
        Some(json!({})),
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
) -> Reply {
    let started = std::time::Instant::now();
    loop {
        let reply = get_record(router, registry, record).await;
        if reply.str_field("status") == wanted {
            return reply;
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

/// A `READY` registry, created through this service's own operations. `body` carries the
/// create request's members.
async fn ready_registry(router: &MultiServiceRouter, case: &str, body: Value) -> (String, String) {
    let created = call(router, "POST", "/registries", Some(body)).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistry must answer 202, got {} {:?}",
        created.status, created.body
    );
    let registry_arn = created.str_field("registryArn").to_string();
    let registry_id = id_of(&registry_arn).to_string();

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
            break;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the registry never became READY, last answer {} {:?}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    (registry_id, registry_arn)
}

/// The members a record in these tests is created with, so every row starts from one
/// known content and what an omitted member leaves alone is visible. `name` is the row's,
/// since "names are unique within a registry".
fn created_with(name: &str) -> Value {
    json!({
        "name": name,
        "displayName": "the display name it was created with",
        "description": "the description it was created with",
        "recordType": "CUSTOM",
        "recordVersion": "1.0.0",
        "descriptors": { "custom": { "data": "{\"created\":true}" } },
        "customMetadata": { "team": "catalogue", "critical": true },
    })
}

/// Create a record named `name` with [`created_with`] and wait for the creation the clock
/// owes, so it is `DRAFT`. Returns (recordId, recordArn).
async fn draft_record(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    name: &str,
) -> (String, String) {
    let created = call(
        router,
        "POST",
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(created_with(name)),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord must answer 202, got {} {:?}",
        created.status, created.body
    );
    let record_arn = created.str_field("recordArn").to_string();
    let record_id = id_of(&record_arn).to_string();
    record_reaching(router, case, registry_id, &record_id, "DRAFT").await;
    (record_id, record_arn)
}

/// Assert that `body` carries exactly `expected` for every member `expected` names, where
/// a `Value::Null` expectation means the member must be ABSENT.
fn assert_members(case: &str, body: &Value, expected: &Map<String, Value>, through: &str) {
    for (member, wanted) in expected {
        let actual = body.get(member).unwrap_or(&Value::Null);
        assert_eq!(
            actual,
            wanted,
            "case {case:?}: {through} must report {member} as {wanted:?}, got {actual:?} \
             (whole body {body:?})"
        );
    }
}

#[tokio::test]
async fn a_patch_is_applied_to_the_stored_record_and_served_by_every_read_path() {
    let (router, _engine) = router();
    let (registry_id, _registry_arn) = ready_registry(
        &router,
        "patch semantics",
        json!({ "name": "patch-semantics-registry" }),
    )
    .await;

    // (case, the PATCH body, what the record carries afterwards). Only the members a row
    // names change: every other member must still report what the record was created
    // with, which is what "omit the field to leave it unchanged" means. A `null`
    // expectation means the member is absent from the answer.
    //
    // The wrapper shapes come from the model: a leaf wrapper is
    // `{"optionalValue": <value>}` and an empty `{}` unsets the field
    // (`smithy_query(query="shape", shape="UpdatedDisplayName,UpdatedDescriptors")`).
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "set displayName",
            json!({ "displayName": { "optionalValue": "a display name set by the update" } }),
            json!({ "displayName": "a display name set by the update" }),
        ),
        (
            "unset displayName with an empty wrapper",
            json!({ "displayName": {} }),
            json!({ "displayName": null }),
        ),
        (
            "set description",
            json!({ "description": { "optionalValue": "a description set by the update" } }),
            json!({ "description": "a description set by the update" }),
        ),
        (
            "unset description with an empty wrapper",
            json!({ "description": {} }),
            json!({ "description": null }),
        ),
        (
            "change recordVersion",
            json!({ "recordVersion": "2.0.0" }),
            json!({ "recordVersion": "2.0.0" }),
        ),
        (
            "patch a descriptor leaf",
            json!({
                "descriptors": {
                    "optionalValue": {
                        "custom": { "optionalValue": { "data": { "optionalValue": "{\"patched\":true}" } } }
                    }
                }
            }),
            json!({ "descriptors": { "custom": { "data": "{\"patched\":true}" } } }),
        ),
        (
            "add a second descriptor of the typed set, leaving the first alone",
            json!({
                "descriptors": {
                    "optionalValue": {
                        "mcpServer": {
                            "optionalValue": {
                                "data": { "optionalValue": "{\"tools\":[]}" },
                                "dataSchemaVersion": { "optionalValue": "2025-06-18" }
                            }
                        }
                    }
                }
            }),
            json!({
                "recordType": "CUSTOM",
                "descriptors": {
                    "custom": { "data": "{\"created\":true}" },
                    "mcpServer": { "data": "{\"tools\":[]}", "dataSchemaVersion": "2025-06-18" }
                }
            }),
        ),
        (
            "patch a descriptor nested two wrappers deep",
            json!({
                "recordType": "MCP",
                "descriptors": {
                    "optionalValue": {
                        "custom": {},
                        "mcpServer": {
                            "optionalValue": {
                                "data": { "optionalValue": "{\"tools\":[\"search\"]}" },
                                "additionalData": {
                                    "optionalValue": {
                                        "tools": {
                                            "optionalValue": {
                                                "data": { "optionalValue": "{\"count\":1}" },
                                                "dataSchemaVersion": { "optionalValue": "2025-06-18" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }),
            json!({
                "recordType": "MCP",
                "descriptors": {
                    "mcpServer": {
                        "data": "{\"tools\":[\"search\"]}",
                        "additionalData": {
                            "tools": { "data": "{\"count\":1}", "dataSchemaVersion": "2025-06-18" }
                        }
                    }
                }
            }),
        ),
        (
            "clear the whole descriptor set with an empty wrapper",
            json!({ "descriptors": {} }),
            json!({ "descriptors": null }),
        ),
        (
            "replace customMetadata",
            json!({ "customMetadata": { "optionalValue": { "team": "platform", "critical": false, "tier": "1" } } }),
            json!({ "customMetadata": { "team": "platform", "critical": false, "tier": "1" } }),
        ),
        (
            "clear customMetadata with an empty wrapper",
            json!({ "customMetadata": {} }),
            json!({ "customMetadata": null }),
        ),
        (
            "patch two members in one request",
            json!({
                "displayName": { "optionalValue": "both at once" },
                "description": {}
            }),
            json!({ "displayName": "both at once", "description": null }),
        ),
    ];

    for (index, (case, patch, changes)) in rows.into_iter().enumerate() {
        let name = format!("patch-probe-{index}");
        let (record_id, record_arn) = draft_record(&router, case, &registry_id, &name).await;

        // What the record must report afterwards: what it was created with, overwritten
        // by the row's expectations.
        let mut expected: Map<String, Value> = created_with(&name)
            .as_object()
            .expect("an object")
            .clone();
        for (member, value) in changes.as_object().expect("an object") {
            expected.insert(member.clone(), value.clone());
        }

        let updated = update_record(&router, &registry_id, &record_id, patch.clone()).await;
        assert_eq!(
            updated.status, 202,
            "case {case:?}: UpdateRegistryRecord must answer 202, got {} {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            (
                updated.str_field("recordId"),
                updated.str_field("recordArn"),
                updated.str_field("status"),
            ),
            (record_id.as_str(), record_arn.as_str(), "UPDATING"),
            "case {case:?}: the answer addresses the record it patched and reports the \
             asynchronous status, got {:?}",
            updated.body
        );
        // The answer already carries the patched content, since it reports the record
        // that was just written.
        assert_members(case, &updated.body, &expected, "the update's own answer");

        // The record settles back at DRAFT (the status it was in), and the read path
        // reports the patch.
        let read_back = record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
        assert_members(case, &read_back.body, &expected, "GetRegistryRecord");

        // The list path projects the SAME stored record, so the patch is visible there
        // too — for the members a `RegistryRecordSummary` carries.
        let listed = list_records(&router, &registry_id).await;
        let summary = listed.body["registryRecords"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|record| record["recordId"].as_str() == Some(record_id.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        let summarised: Map<String, Value> = ["name", "displayName", "description", "recordType", "recordVersion"]
            .into_iter()
            .map(|member| {
                (
                    member.to_string(),
                    expected.get(member).cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        assert_members(case, &summary, &summarised, "the ListRegistryRecords summary");
    }
}

#[tokio::test]
async fn a_rename_is_persisted_and_keeps_record_names_unique_in_the_registry() {
    let (router, _engine) = router();
    let (registry_id, _registry_arn) = ready_registry(
        &router,
        "rename",
        json!({ "name": "rename-registry" }),
    )
    .await;
    let (record_id, _record_arn) = draft_record(&router, "rename", &registry_id, "first").await;
    let (other_id, _other_arn) = draft_record(&router, "rename", &registry_id, "second").await;

    // Rows span the names a rename can ask for: a free one, the record's own name (the
    // same record, so not a duplicate), and one another record of the registry holds.
    let rows: Vec<(&str, &str, Option<u16>)> = vec![
        ("a name no record holds", "first-renamed", None),
        ("the name the record already holds", "first-renamed", None),
        ("a name another record of the registry holds", "second", Some(409)),
    ];
    for (case, wanted, refused_with) in rows {
        let answered = update_record(
            &router,
            &registry_id,
            &record_id,
            json!({ "name": wanted }),
        )
        .await;
        match refused_with {
            None => {
                assert_eq!(
                    (answered.status, answered.str_field("name")),
                    (202, wanted),
                    "case {case:?}: the rename is accepted and answered, got {} {:?}",
                    answered.status,
                    answered.body
                );
                let read_back =
                    record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
                assert_eq!(
                    read_back.str_field("name"),
                    wanted,
                    "case {case:?}: a read reports the renamed record, got {:?}",
                    read_back.body
                );
            }
            Some(status) => {
                assert_eq!(
                    (
                        answered.status,
                        answered.error_code.as_deref(),
                        answered.message()
                    ),
                    (
                        status,
                        Some("ConflictException"),
                        control::duplicate_record_name_message(wanted, &registry_id)
                    ),
                    "case {case:?}: expected this service's duplicate-name refusal, got {} \
                     {:?}",
                    answered.status,
                    answered.body
                );
                // The refused rename wrote nothing: the other record keeps its name and
                // this one keeps the name it had.
                let read_back = get_record(&router, &registry_id, &record_id).await;
                assert_eq!(
                    read_back.str_field("name"),
                    "first-renamed",
                    "case {case:?}: a refused rename leaves the stored name alone, got {:?}",
                    read_back.body
                );
                let untouched = get_record(&router, &registry_id, &other_id).await;
                assert_eq!(
                    untouched.str_field("name"),
                    "second",
                    "case {case:?}: a refused rename leaves the other record alone, got {:?}",
                    untouched.body
                );
            }
        }
    }
}

/// One row of the edit-settles-where table: the status a record is in when an edit
/// arrives, and where the edit leaves it.
struct EditRow {
    case: &'static str,
    /// Whether the record's registry approves a submission instead of sending it to
    /// review, which is how a row reaches `APPROVED`.
    auto_approve: bool,
    /// The status the record is in when the edit arrives.
    start_status: &'static str,
    /// The reason it carries then, set with the status when the row needs one.
    start_reason: Option<&'static str>,
    /// The status the record settles at once the update has been processed.
    settles_at: &'static str,
    /// The reason it carries afterwards.
    reason_after: Option<&'static str>,
}

#[tokio::test]
async fn an_edit_settles_the_record_at_the_status_its_curation_state_decides() {
    let (router, _engine) = router();

    // An edit to a record under review takes it back out of the approval workflow; any
    // other record settles back where it was.
    let rows: Vec<EditRow> = vec![
        EditRow {
            case: "a draft record",
            auto_approve: false,
            start_status: "DRAFT",
            start_reason: None,
            settles_at: "DRAFT",
            reason_after: None,
        },
        EditRow {
            case: "a record waiting for review",
            auto_approve: false,
            start_status: "PENDING_APPROVAL",
            start_reason: None,
            settles_at: "DRAFT",
            reason_after: None,
        },
        EditRow {
            case: "a rejected record",
            auto_approve: true,
            start_status: "REJECTED",
            start_reason: Some("descriptor does not describe a reachable endpoint"),
            settles_at: "DRAFT",
            reason_after: None,
        },
        EditRow {
            case: "an approved record",
            auto_approve: true,
            start_status: "APPROVED",
            start_reason: Some("reviewed by the catalogue team"),
            settles_at: "APPROVED",
            reason_after: Some("reviewed by the catalogue team"),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let EditRow {
            case,
            auto_approve,
            start_status,
            start_reason,
            settles_at,
            reason_after,
        } = row;
        let mut registry_body = json!({ "name": format!("edit-registry-{index}") });
        if auto_approve {
            registry_body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
        }
        let (registry_id, _registry_arn) = ready_registry(&router, case, registry_body).await;
        let (record_id, _record_arn) = draft_record(&router, case, &registry_id, "edited").await;

        if start_status != "DRAFT" {
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
                "case {case:?}: SubmitRegistryRecordForApproval must answer 202, got {} {:?}",
                submitted.status, submitted.body
            );
            if let Some(reason) = start_reason {
                let moved = call(
                    &router,
                    "PATCH",
                    &format!(
                        "/registries/{}/records/{}/status",
                        encode_label(&registry_id),
                        encode_label(&record_id)
                    ),
                    Some(json!({ "status": start_status, "statusReason": reason })),
                )
                .await;
                assert_eq!(
                    moved.status, 202,
                    "case {case:?}: UpdateRegistryRecordStatus must answer 202, got {} {:?}",
                    moved.status, moved.body
                );
            }
            record_reaching(&router, case, &registry_id, &record_id, start_status).await;
        }

        let updated = update_record(
            &router,
            &registry_id,
            &record_id,
            json!({ "displayName": { "optionalValue": "edited while in the workflow" } }),
        )
        .await;
        assert_eq!(
            (updated.status, updated.str_field("status")),
            (202, "UPDATING"),
            "case {case:?}: an accepted update reports the asynchronous status, got {} {:?}",
            updated.status,
            updated.body
        );

        let settled = record_reaching(&router, case, &registry_id, &record_id, settles_at).await;
        assert_eq!(
            settled.str_field("displayName"),
            "edited while in the workflow",
            "case {case:?}: the settled record carries the edit, got {:?}",
            settled.body
        );
        assert_eq!(
            settled.body.get("statusReason").and_then(Value::as_str),
            reason_after,
            "case {case:?}: expected statusReason {reason_after:?} after the edit, got {:?}",
            settled.body
        );
    }
}

#[tokio::test]
async fn an_update_this_service_cannot_apply_is_refused_and_writes_nothing() {
    let (router, engine) = router();
    let (registry_id, registry_arn) = ready_registry(
        &router,
        "refusals",
        json!({ "name": "refusal-registry" }),
    )
    .await;
    let (record_id, record_arn) = draft_record(&router, "refusals", &registry_id, "refused").await;

    // A registry id and a record id of the modelled shapes (`RegistryId`
    // ^[a-zA-Z0-9]{12,16}$, `RegistryRecordId` ^[a-zA-Z0-9]{12}$) that this service holds
    // nothing under, each given bare and as the resource's full ARN, so the handler and
    // not the boundary validator is what refuses them.
    let absent = "zzz999999999";
    let absent_registry_arn =
        format!("arn:aws:agent-registry:{REGION}:000000000000:registry/{absent}");
    let absent_record_arn = format!("{registry_arn}/record/{absent}");
    let not_found: Vec<(&str, String, String, String)> = vec![
        (
            "a registry this service holds nothing under",
            absent.to_string(),
            record_id.clone(),
            control::registry_not_found_message(absent),
        ),
        (
            "the same registry named by its ARN",
            absent_registry_arn.clone(),
            record_id.clone(),
            control::registry_not_found_message(&absent_registry_arn),
        ),
        (
            "a record this registry holds nothing under",
            registry_id.clone(),
            absent.to_string(),
            control::record_not_found_message(absent),
        ),
        (
            "the same record named by its ARN",
            registry_arn.clone(),
            absent_record_arn.clone(),
            control::record_not_found_message(absent),
        ),
    ];
    for (case, registry, record, expected) in not_found {
        let refused = update_record(
            &router,
            &registry,
            &record,
            json!({ "displayName": { "optionalValue": "never stored" } }),
        )
        .await;
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (404, Some("ResourceNotFoundException"), expected.clone()),
            "case {case:?}: expected the modelled 404 with this service's own sentence \
             ({expected:?}), got {} {:?}",
            refused.status,
            refused.body
        );
    }

    // `provenance` is reserved for the service principal that auto-detects records, so a
    // caller asserting a lineage for itself is refused. The entry satisfies
    // `Provenance`'s own members (`relation`, `sourceId` matching its ARN pattern).
    let forbidden = update_record(
        &router,
        &registry_id,
        &record_id,
        json!({
            "provenance": [{
                "relation": "DETECTED_FROM",
                "sourceId": format!("arn:aws:bedrock-agentcore:{REGION}:000000000000:runtime/abc"),
            }]
        }),
    )
    .await;
    assert_eq!(
        (forbidden.status, forbidden.error_code.as_deref()),
        (403, Some("AccessDeniedException")),
        "case \"provenance from another caller\": expected the modelled 403, got {} {:?}",
        forbidden.status,
        forbidden.body
    );

    // A record whose own asynchronous change is still in flight has no settled content to
    // patch. Only this service's own work puts a record in one of those statuses, so each
    // row is staged in its store directly.
    for in_flight in ["CREATING", "UPDATING"] {
        {
            let state = engine
                .state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
            let mut state = state.lock();
            let record = state
                .registries
                .get_mut(&registry_id)
                .and_then(|registry| registry.records.get_mut(&record_id))
                .expect("the record the suite created");
            record.status = in_flight.to_string();
            record.pending = None;
        }
        let refused = update_record(
            &router,
            &registry_id,
            &record_id,
            json!({ "displayName": { "optionalValue": "never stored" } }),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (409, Some("ConflictException")),
            "case \"a record in {in_flight}\": expected the modelled 409 ConflictException, \
             got {} {:?}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(in_flight),
            "case \"a record in {in_flight}\": the refusal names the status the record is \
             in, got {:?}",
            refused.message()
        );
    }

    // A retired record is in the lifecycle's terminal status, so no later change applies to
    // it — the sentence this service states for that class, from its one place.
    {
        let state =
            engine.state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
        let mut state = state.lock();
        let record = state
            .registries
            .get_mut(&registry_id)
            .and_then(|registry| registry.records.get_mut(&record_id))
            .expect("the record the suite created");
        record.status = "DEPRECATED".to_string();
        record.pending = None;
    }
    let retired = update_record(
        &router,
        &registry_id,
        &record_id,
        json!({ "displayName": { "optionalValue": "never stored" } }),
    )
    .await;
    assert_eq!(
        (
            retired.status,
            retired.error_code.as_deref(),
            retired.message()
        ),
        (
            400,
            Some("ValidationException"),
            control::terminal_record_status_message("DEPRECATED")
        ),
        "case \"a retired record\": expected the modelled 400 with this service's terminal \
         sentence, got {} {:?}",
        retired.status,
        retired.body
    );

    // None of the refusals above wrote anything: the record still carries what it was
    // created with.
    {
        let state =
            engine.state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
        let mut state = state.lock();
        let record = state
            .registries
            .get_mut(&registry_id)
            .and_then(|registry| registry.records.get_mut(&record_id))
            .expect("the record the suite created");
        record.status = control::RECORD_DRAFT.to_string();
    }
    let read_back = get_record(&router, &registry_id, &record_arn).await;
    assert_eq!(
        read_back.str_field("displayName"),
        "the display name it was created with",
        "case \"after every refusal\": a refused update leaves the stored record alone, \
         got {:?}",
        read_back.body
    );
}

#[tokio::test]
async fn record_names_are_unique_within_a_registry_and_not_across_registries() {
    let (router, _engine) = router();
    let (registry_id, registry_arn) = ready_registry(
        &router,
        "name scope",
        json!({ "name": "name-scope-registry" }),
    )
    .await;
    let (other_registry_id, _other_registry_arn) = ready_registry(
        &router,
        "name scope",
        json!({ "name": "name-scope-other-registry" }),
    )
    .await;
    let (record_id, _record_arn) = draft_record(&router, "name scope", &registry_id, "alpha").await;
    let (sibling_id, _sibling_arn) =
        draft_record(&router, "name scope", &registry_id, "beta").await;
    // A record of the OTHER registry, holding a name this record is renamed to below: the
    // scope a record name is unique within is one registry, so that name is free here.
    let (neighbour_id, _neighbour_arn) =
        draft_record(&router, "name scope", &other_registry_id, "gamma").await;

    // Rows span the names a rename can ask for and the identifier form the record's
    // registry is addressed by: a name nothing holds, the name the record already holds,
    // the name a sibling of the same registry holds (bare id and ARN), and a name only a
    // record of another registry holds.
    let rows: Vec<(&str, bool, &str, bool)> = vec![
        (
            "a name no record of the registry holds",
            false,
            "delta",
            false,
        ),
        ("the name the record already holds", false, "delta", false),
        (
            "a name a sibling of the same registry holds",
            false,
            "beta",
            true,
        ),
        (
            "the same clash with the registry given as its ARN",
            true,
            "beta",
            true,
        ),
        (
            "a name only a record of another registry holds",
            false,
            "gamma",
            false,
        ),
    ];

    for (case, by_arn, wanted, refused) in rows {
        let registry = if by_arn {
            registry_arn.as_str()
        } else {
            registry_id.as_str()
        };
        let answered =
            update_record(&router, registry, &record_id, json!({ "name": wanted })).await;
        if refused {
            let expected = control::duplicate_record_name_message(wanted, registry);
            assert_eq!(
                (
                    answered.status,
                    answered.error_code.as_deref(),
                    answered.message()
                ),
                (409, Some("ConflictException"), expected.clone()),
                "case {case:?}: expected this service's duplicate-name refusal \
                 ({expected:?}), got {} {:?}",
                answered.status,
                answered.body
            );
        } else {
            assert_eq!(
                (answered.status, answered.str_field("name")),
                (202, wanted),
                "case {case:?}: the rename is accepted and answered, got {} {:?}",
                answered.status,
                answered.body
            );
            let settled = record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
            assert_eq!(
                settled.str_field("name"),
                wanted,
                "case {case:?}: a read reports the renamed record, got {:?}",
                settled.body
            );
        }
    }

    // No row touched another record: the sibling keeps the name a refused rename asked for,
    // and the record of the other registry keeps the name an accepted rename took.
    let sibling = get_record(&router, &registry_id, &sibling_id).await;
    assert_eq!(
        sibling.str_field("name"),
        "beta",
        "case \"after every rename\": the sibling keeps its own name, got {:?}",
        sibling.body
    );
    let neighbour = get_record(&router, &other_registry_id, &neighbour_id).await;
    assert_eq!(
        neighbour.str_field("name"),
        "gamma",
        "case \"after every rename\": the record of the other registry is untouched, got \
         {:?}",
        neighbour.body
    );
}

/// Put a registry into `status` directly in this service's store: only this service's own
/// asynchronous work reaches `CREATING` and `DELETING`, and neither lasts long enough for a
/// test to address one of the registry's records while it holds.
fn set_registry_status(engine: &Arc<engine::InMemoryEngine>, registry_id: &str, status: &str) {
    let state =
        engine.state::<control::ComAmazonawsAgentregistrycontrolAgentregistrycontrolState>();
    let mut state = state.lock();
    let registry = state
        .registries
        .get_mut(registry_id)
        .expect("the registry the suite created");
    registry.status = status.to_string();
    registry.pending = None;
}

/// One row of the registry-state table: the status the record's registry is in when the
/// edit arrives, and whether the edit is accepted then.
struct RegistryStateRow {
    case: &'static str,
    registry_status: &'static str,
    accepted: bool,
}

#[tokio::test]
async fn an_edit_of_a_record_is_held_to_its_registry_being_ready() {
    let (router, engine) = router();
    let (registry_id, _registry_arn) = ready_registry(
        &router,
        "registry state",
        json!({ "name": "registry-state-registry" }),
    )
    .await;
    let (record_id, _record_arn) =
        draft_record(&router, "registry state", &registry_id, "held").await;

    // Rows span the statuses a registry can be in when an edit of one of its records
    // arrives: the two transient ones and the usable one.
    let rows: Vec<RegistryStateRow> = vec![
        RegistryStateRow {
            case: "a registry still being created",
            registry_status: control::REGISTRY_CREATING,
            accepted: false,
        },
        RegistryStateRow {
            case: "a registry being deleted",
            registry_status: control::REGISTRY_DELETING,
            accepted: false,
        },
        RegistryStateRow {
            case: "a ready registry",
            registry_status: control::REGISTRY_READY,
            accepted: true,
        },
    ];

    for row in rows {
        let RegistryStateRow {
            case,
            registry_status,
            accepted,
        } = row;
        set_registry_status(&engine, &registry_id, registry_status);
        let wanted = format!("edited with the registry in {registry_status}");
        let answered = update_record(
            &router,
            &registry_id,
            &record_id,
            json!({ "displayName": { "optionalValue": wanted } }),
        )
        .await;
        if accepted {
            assert_eq!(
                (answered.status, answered.str_field("status")),
                (202, "UPDATING"),
                "case {case:?}: an accepted update reports the asynchronous status, got {} \
                 {:?}",
                answered.status,
                answered.body
            );
            let settled = record_reaching(&router, case, &registry_id, &record_id, "DRAFT").await;
            assert_eq!(
                settled.str_field("displayName"),
                wanted,
                "case {case:?}: the settled record carries the edit, got {:?}",
                settled.body
            );
        } else {
            assert_eq!(
                (answered.status, answered.error_code.as_deref()),
                (409, Some("ConflictException")),
                "case {case:?}: expected the modelled 409 ConflictException, got {} {:?}",
                answered.status,
                answered.body
            );
            assert!(
                answered.message().contains(registry_status),
                "case {case:?}: the refusal names the status the registry is in, got {:?}",
                answered.message()
            );
            // The refused edit wrote nothing: with the registry usable again the record
            // still carries the display name it was created with.
            set_registry_status(&engine, &registry_id, control::REGISTRY_READY);
            let read_back = get_record(&router, &registry_id, &record_id).await;
            assert_eq!(
                read_back.str_field("displayName"),
                "the display name it was created with",
                "case {case:?}: a refused update leaves the stored record alone, got {:?}",
                read_back.body
            );
        }
    }
}
