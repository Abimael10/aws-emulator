// SPDX-License-Identifier: CC0-1.0

//! Editing an APPROVED Agent Registry record, on the ASSEMBLED emulator — every service
//! mounted, as `aws_emulator_router::build_router` assembles the one the binary serves, so
//! the Agent Registry DATA plane answers beside the CONTROL plane over the one shared store.
//!
//! THE RULE THESE SUITES PIN: a registry record enters the discovery surface when the
//! registry's approval workflow approves it, and what that surface then serves is the
//! revision the approval published — the content the record held when it was last approved.
//! Editing the record's content therefore invalidates the review it was made against: the
//! record is withdrawn from the approval workflow (it passes through `UPDATING` and settles
//! at `DRAFT`, carrying the edited content for the control plane's own reads), the edited
//! content is NOT published, and consumers keep resolving the last approved revision until
//! that edit is submitted and approved in turn. The published revision is replaced only by
//! the next approval, and withdrawn only by a curation step that takes the record off the
//! discovery surface.
//!
//! The rows span what that rule generalises over. The first suite varies the MEMBER the edit
//! patches — a nested descriptor payload, the two optional members a patch wrapper can
//! unset, and the two plain members a patch replaces outright — so no single member or
//! wrapper form satisfies it, and reads each row back through the control plane, the data
//! plane's batch read and its listing. The second varies the curation step that follows the
//! approval, including one taken while an edit stands withdrawn, so a published revision is
//! shown to be withdrawn by the workflow rather than immortal.
//!
//! Nothing here seeds a store directly: every registry and record is authored through the
//! control plane's own operations and read back through both planes, so neither plane is
//! asserted against a copy of the other's state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below signs for.
const REGION: &str = "us-west-2";

/// The access key id every call signs with: a key neither STS nor IAM holds a record for,
/// which is the position AWS account root credentials are in, so no identity-based policy
/// bounds these reads.
const ACCOUNT_KEY_ID: &str = "AKIDTESTAGENTREGISTRY";

/// How long a wait for a transition the clock owes may take before a suite gives up.
/// Generous on purpose: the wait is on the CONDITION, so a loaded host only polls more.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The `RegistryRecordStatus` a record is discoverable in, and the one the discovery surface
/// reports for the revision an approval published.
const DISCOVERABLE_STATUS: &str = "APPROVED";

/// The `RegistryRecordStatus` an edit withdraws a record from the approval workflow to.
const WITHDRAWN_STATUS: &str = "DRAFT";

/// The `RegistryRecordStatus` the asynchronous edit reports while it is being processed.
const UPDATING_STATUS: &str = "UPDATING";

/// The `BatchGetDiscoverableRegistryRecordErrorCode` reported for a record the batch read
/// could not return (the enum also declares ACCESS_DENIED and INTERNAL_ERROR).
const RESOURCE_NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// The descriptor payload a record is created with, and the one its first approval
/// publishes.
const APPROVED_PAYLOAD: &str = "{\"rev\":1}";

/// The descriptor payload the editing row patches it to.
const EDITED_PAYLOAD: &str = "{\"rev\":2}";

/// One answer from the assembled emulator, reduced to what these suites assert on.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    /// The summary this answer carries for `record_id`, when it carries one.
    fn summary_of(&self, record_id: &str) -> Option<Value> {
        self.body["registryRecords"]
            .as_array()?
            .iter()
            .find(|record| record["recordId"].as_str() == Some(record_id))
            .cloned()
    }

    /// The `errors` entries this answer carries for `record_id`.
    fn errors_for(&self, record_id: &str) -> Vec<Value> {
        self.body["errors"]
            .as_array()
            .map(|errors| {
                errors
                    .iter()
                    .filter(|error| error["recordId"].as_str() == Some(record_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The value at a dotted member path of one response object (`descriptors.custom.data`), or
/// `Value::Null` where the path names nothing.
fn member_at(body: &Value, path: &str) -> Value {
    let mut current = body;
    for segment in path.split('.') {
        current = match current.get(segment) {
            Some(value) => value,
            None => return Value::Null,
        };
    }
    current.clone()
}

/// The whole emulator, exactly as the binary assembles it, over one shared engine. Request
/// authentication is off: these suites are about what a request is ANSWERED with, and the
/// signature of a hand-built request is not checked anywhere in this target.
fn assembled_emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    let payload = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://agent-registry.{REGION}.amazonaws.com{path}"))
        .header("host", format!("agent-registry.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/agent-registry/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// The last `/`-separated segment of an ARN: a registry id, or a record id.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

// --- the control plane, as a client calls it ------------------------------------------

/// A registry whose `approvalConfiguration.autoApprovalRules` carry `APPROVE_ALL`, the one
/// rule the model declares, so a submission is approved rather than left for a curator.
async fn create_auto_approving_registry(router: &MultiServiceRouter, name: &str) -> Reply {
    call(
        router,
        "POST",
        "/registries",
        Some(json!({
            "name": name,
            "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] },
        })),
    )
    .await
}

async fn get_registry(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(router, "GET", &format!("/registries/{registry_id}"), None).await
}

/// A `CUSTOM` record holding the content a first approval publishes: the members the rows
/// below patch are all present, so each row's patch REPLACES a stored value.
async fn create_record(router: &MultiServiceRouter, registry_id: &str, name: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/records"),
        Some(json!({
            "name": name,
            "displayName": "approved display name",
            "description": "approved description",
            "recordType": "CUSTOM",
            "recordVersion": "1.0.0",
            "descriptors": { "custom": { "data": APPROVED_PAYLOAD } },
        })),
    )
    .await
}

async fn get_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/registries/{registry_id}/records/{record_id}"),
        None,
    )
    .await
}

async fn submit_record(router: &MultiServiceRouter, registry_id: &str, record_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/records/{record_id}/submit-for-approval"),
        None,
    )
    .await
}

async fn update_record(
    router: &MultiServiceRouter,
    registry_id: &str,
    record_id: &str,
    patch: Value,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!("/registries/{registry_id}/records/{record_id}"),
        Some(patch),
    )
    .await
}

async fn update_status(
    router: &MultiServiceRouter,
    registry_id: &str,
    record_id: &str,
    status: &str,
    reason: &str,
) -> Reply {
    call(
        router,
        "PATCH",
        &format!("/registries/{registry_id}/records/{record_id}/status"),
        Some(json!({ "status": status, "statusReason": reason })),
    )
    .await
}

// --- the data plane's discovery reads --------------------------------------------------

async fn list_discoverable(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/discoverable-records-list"),
        Some(json!({ "maxResults": 100 })),
    )
    .await
}

async fn batch_get_discoverable(
    router: &MultiServiceRouter,
    registry_id: &str,
    record_id: &str,
) -> Reply {
    call(
        router,
        "POST",
        "/discoverable-records-batch",
        Some(json!({ "entries": [{ "registryId": registry_id, "recordIds": [record_id] }] })),
    )
    .await
}

// --- waits on a transition the clock owes ---------------------------------------------

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

/// Look at a record until the CONTROL plane reports `wanted`, or give up at the deadline.
/// Answers the reply that reported it, so a caller asserts on the settled record.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) -> Reply {
    let started = Instant::now();
    loop {
        let reply = get_record(router, registry_id, record_id).await;
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

/// One record of the registry, created and carried to `APPROVED` through the control plane's
/// own operations: a create, the clock-owed `CREATING` -> `DRAFT`, and a submission the
/// registry approves. Answers its record id.
async fn approved_record(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    name: &str,
) -> String {
    let created = create_record(router, registry_id, name).await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
        created.status, created.body
    );
    let record_id = id_of(created.str_field("recordArn")).to_string();
    record_reaching(router, case, registry_id, &record_id, WITHDRAWN_STATUS).await;

    let submitted = submit_record(router, registry_id, &record_id).await;
    assert_eq!(
        (submitted.status, submitted.str_field("status")),
        (202, DISCOVERABLE_STATUS),
        "case {case:?}: a submission to an auto-approving registry approves the record, \
         got {} {:?}",
        submitted.status,
        submitted.body
    );
    record_id
}

/// What the data plane serves for `record_id` at the dotted member `path`, read through the
/// batch read: the member's value, or `Value::Null` when the record is not on the surface.
async fn served_member(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    path: &str,
) -> Value {
    let reply = batch_get_discoverable(router, registry_id, record_id).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: BatchGetDiscoverableRegistryRecord answers 200, got {} {:?}",
        reply.status, reply.body
    );
    match reply.summary_of(record_id) {
        Some(summary) => member_at(&summary, path),
        None => Value::Null,
    }
}

/// One row of the edit table: the member the edit patches, the patch body, and the value the
/// discovery surface serves for that member before and after the edit is approved.
struct EditRow {
    case: &'static str,
    /// The record's name in the shared registry, which is unique within it.
    name: &'static str,
    /// The dotted member path of the discovery summary the row reads back.
    path: &'static str,
    /// The `UpdateRegistryRecord` body: `displayName`, `description` and `descriptors` are
    /// patched through their update wrappers; `name` and `recordVersion` are patched as the
    /// model declares them, by the value itself.
    patch: Value,
    /// What the first approval published for that member.
    approved: Value,
    /// What the edit leaves on the record, and what the next approval publishes.
    edited: Value,
}

#[tokio::test]
async fn an_edit_withdraws_the_record_without_withdrawing_its_approved_revision() {
    let router = assembled_emulator();

    let registry = create_auto_approving_registry(&router, "approved-revision-edits").await;
    assert_eq!(
        registry.status, 202,
        "CreateRegistry answers 202, got {} {:?}",
        registry.status, registry.body
    );
    let registry_id = id_of(registry.str_field("registryArn")).to_string();
    registry_reaching(&router, "the shared registry", &registry_id, "READY").await;

    let rows: Vec<EditRow> = vec![
        EditRow {
            case: "the custom descriptor payload",
            name: "revision-descriptor",
            path: "descriptors.custom.data",
            patch: json!({
                "descriptors": {
                    "optionalValue": {
                        "custom": { "optionalValue": { "data": { "optionalValue": EDITED_PAYLOAD } } }
                    }
                }
            }),
            approved: json!(APPROVED_PAYLOAD),
            edited: json!(EDITED_PAYLOAD),
        },
        EditRow {
            case: "the display name",
            name: "revision-display-name",
            path: "displayName",
            patch: json!({ "displayName": { "optionalValue": "edited display name" } }),
            approved: json!("approved display name"),
            edited: json!("edited display name"),
        },
        EditRow {
            case: "the description",
            name: "revision-description",
            path: "description",
            patch: json!({ "description": { "optionalValue": "edited description" } }),
            approved: json!("approved description"),
            edited: json!("edited description"),
        },
        EditRow {
            case: "the record version",
            name: "revision-version",
            path: "recordVersion",
            patch: json!({ "recordVersion": "2.0.0" }),
            approved: json!("1.0.0"),
            edited: json!("2.0.0"),
        },
        EditRow {
            case: "the record name",
            name: "revision-name",
            path: "name",
            patch: json!({ "name": "revision-name-edited" }),
            approved: json!("revision-name"),
            edited: json!("revision-name-edited"),
        },
    ];

    for row in rows {
        let EditRow {
            case,
            name,
            path,
            patch,
            approved,
            edited,
        } = row;
        let record_id = approved_record(&router, case, &registry_id, name).await;

        // The approval published the record: the data plane serves the approved revision.
        assert_eq!(
            served_member(&router, case, &registry_id, &record_id, path).await,
            approved,
            "case {case:?}: the approved revision is what the discovery surface serves for \
             {path}"
        );

        // The edit is accepted and asynchronous.
        let updated = update_record(&router, &registry_id, &record_id, patch).await;
        assert_eq!(
            (updated.status, updated.str_field("status")),
            (202, UPDATING_STATUS),
            "case {case:?}: an accepted edit answers 202 with the asynchronous status, \
             got {} {:?}",
            updated.status,
            updated.body
        );

        // It withdraws the record from the approval workflow: the record settles at DRAFT
        // and the control plane reports the edited content under that status.
        let settled =
            record_reaching(&router, case, &registry_id, &record_id, WITHDRAWN_STATUS).await;
        assert_eq!(
            member_at(&settled.body, path),
            edited,
            "case {case:?}: the withdrawn record carries the edited {path}, got {:?}",
            settled.body
        );
        assert_eq!(
            settled.body.get("statusReason").and_then(Value::as_str),
            None,
            "case {case:?}: a withdrawn record claims no reason for the status it left, \
             got {:?}",
            settled.body
        );

        // The edited content is not published: consumers keep resolving the revision the
        // approval published, under the status that revision was approved in, and the
        // record stays on the surface the listing reports.
        let served = batch_get_discoverable(&router, &registry_id, &record_id).await;
        let summary = served.summary_of(&record_id).unwrap_or_else(|| {
            panic!(
                "case {case:?}: a withdrawn edit leaves the approved revision discoverable, \
                 got {:?}",
                served.body
            )
        });
        assert_eq!(
            member_at(&summary, path),
            approved,
            "case {case:?}: the surface still serves the approved {path}, got {summary:?}"
        );
        assert_eq!(
            summary["status"].as_str().unwrap_or_default(),
            DISCOVERABLE_STATUS,
            "case {case:?}: the published revision is served as approved content, \
             got {summary:?}"
        );
        let listed = list_discoverable(&router, &registry_id).await;
        assert!(
            listed.summary_of(&record_id).is_some(),
            "case {case:?}: the listing still reports the published record, got {:?}",
            listed.body
        );

        // Submitting the edit again publishes it: the next approval replaces the revision.
        let resubmitted = submit_record(&router, &registry_id, &record_id).await;
        assert_eq!(
            (resubmitted.status, resubmitted.str_field("status")),
            (202, DISCOVERABLE_STATUS),
            "case {case:?}: the withdrawn record can be submitted again, got {} {:?}",
            resubmitted.status,
            resubmitted.body
        );
        assert_eq!(
            served_member(&router, case, &registry_id, &record_id, path).await,
            edited,
            "case {case:?}: the re-approved revision is what the surface serves for {path}"
        );
    }
}

/// One row of the withdrawal table: the curation step taken after the approval, whether an
/// edit stands withdrawn when it is taken, and the status the record ends in.
struct WithdrawalRow {
    case: &'static str,
    name: &'static str,
    /// Whether the record is edited — and so withdrawn to `DRAFT` — before the step.
    edit_first: bool,
    /// The curation step, or `None` for a record no step is taken on.
    step: Option<&'static str>,
}

#[tokio::test]
async fn a_curation_step_off_the_discovery_surface_withdraws_the_published_revision() {
    let router = assembled_emulator();

    let registry = create_auto_approving_registry(&router, "approved-revision-withdrawals").await;
    assert_eq!(
        registry.status, 202,
        "CreateRegistry answers 202, got {} {:?}",
        registry.status, registry.body
    );
    let registry_id = id_of(registry.str_field("registryArn")).to_string();
    registry_reaching(&router, "the shared registry", &registry_id, "READY").await;

    // The steps the lifecycle allows from an approved record which take it off the discovery
    // surface, one of them taken while an edit stands withdrawn (`DRAFT` allows retirement
    // too), plus the record no approval ever published.
    let rows: [WithdrawalRow; 4] = [
        WithdrawalRow {
            case: "retired after approval",
            name: "withdrawal-deprecated",
            edit_first: false,
            step: Some("DEPRECATED"),
        },
        WithdrawalRow {
            case: "turned down after approval",
            name: "withdrawal-rejected",
            edit_first: false,
            step: Some("REJECTED"),
        },
        WithdrawalRow {
            case: "retired while an edit stands withdrawn",
            name: "withdrawal-edited-then-deprecated",
            edit_first: true,
            step: Some("DEPRECATED"),
        },
        WithdrawalRow {
            case: "approved and left alone",
            name: "withdrawal-untouched",
            edit_first: false,
            step: None,
        },
    ];

    for row in rows {
        let WithdrawalRow {
            case,
            name,
            edit_first,
            step,
        } = row;
        let record_id = approved_record(&router, case, &registry_id, name).await;
        assert_eq!(
            served_member(&router, case, &registry_id, &record_id, "recordId").await,
            json!(record_id),
            "case {case:?}: the approval publishes the record"
        );

        if edit_first {
            let updated = update_record(
                &router,
                &registry_id,
                &record_id,
                json!({ "description": { "optionalValue": "edited before the step" } }),
            )
            .await;
            assert_eq!(
                updated.status, 202,
                "case {case:?}: the edit answers 202, got {} {:?}",
                updated.status, updated.body
            );
            record_reaching(&router, case, &registry_id, &record_id, WITHDRAWN_STATUS).await;
        }

        let Some(step) = step else {
            // No step was taken, so the revision the approval published is still served.
            assert_eq!(
                served_member(&router, case, &registry_id, &record_id, "recordId").await,
                json!(record_id),
                "case {case:?}: a record no step was taken on stays on the surface"
            );
            continue;
        };

        let moved = update_status(
            &router,
            &registry_id,
            &record_id,
            step,
            "driven by the suite",
        )
        .await;
        assert_eq!(
            (moved.status, moved.str_field("status")),
            (202, step),
            "case {case:?}: the curation step to {step} answers 202, got {} {:?}",
            moved.status,
            moved.body
        );

        // The step took the record off the discovery surface, so the revision it had
        // published is withdrawn with it: the batch read reports the record as one it could
        // not retrieve, and the listing no longer carries it.
        let served = batch_get_discoverable(&router, &registry_id, &record_id).await;
        assert_eq!(
            served.summary_of(&record_id),
            None,
            "case {case:?}: a record in {step} is served no longer, got {:?}",
            served.body
        );
        let errors = served.errors_for(&record_id);
        assert_eq!(
            errors
                .first()
                .and_then(|error| error["errorCode"].as_str())
                .unwrap_or_default(),
            RESOURCE_NOT_FOUND_CODE,
            "case {case:?}: the batch read reports it under {RESOURCE_NOT_FOUND_CODE}, \
             got {errors:?}"
        );
        let listed = list_discoverable(&router, &registry_id).await;
        assert_eq!(
            listed.summary_of(&record_id),
            None,
            "case {case:?}: the listing no longer carries a record in {step}, got {:?}",
            listed.body
        );
    }
}
