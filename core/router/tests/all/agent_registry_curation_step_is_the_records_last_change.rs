// SPDX-License-Identifier: CC0-1.0

//! The Agent Registry CONTROL plane's curation step, `UpdateRegistryRecordStatus`, on the
//! ASSEMBLED emulator — every service mounted, as `aws_emulator_router::build_router`
//! assembles the one the binary serves, so the DATA plane answers beside it over the one
//! shared store.
//!
//! THE RULE THIS SUITE PINS: the curation step is a WRITE of one record's status, so the
//! status, the reason and the instant it answers with are that record's stored last change —
//! and every later read resolves them from the one store that owns the record. The control
//! plane's `GetRegistryRecord` and `ListRegistryRecords` report them, a listing FILTERED by
//! status selects the record under the status the step wrote and no longer under the one it
//! left, and the data plane's three discovery reads report the record exactly while that
//! stored status is `APPROVED`: "a record is `APPROVED` once it is approved and
//! discoverable", while "`REJECTED` and `DEPRECATED` records are not discoverable" and
//! `DRAFT` / `PENDING_APPROVAL` describe one that has not become discoverable yet
//! (`RegistryRecordStatus`, `smithy_query(query="shape")`).
//!
//! The rows span what that rule generalises over: the status the record stands in when the
//! step is taken, the status the step moves it to (one that makes the record discoverable,
//! two that take it off the discovery surface, and the one it already holds), the reason
//! text carried with it, and whether the record was discoverable before the step. Nothing
//! here seeds a store directly: every record is authored and moved through the control
//! plane's own operations, and read back through both planes, so neither plane is asserted
//! against a copy of the other's state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below signs for.
const REGION: &str = "us-west-2";

/// The access key id every row signs with: a key neither STS nor IAM holds a record for,
/// which is the position AWS account root credentials are in, so no identity-based policy
/// bounds these reads.
const ACCOUNT_KEY_ID: &str = "AKIDTESTAGENTREGISTRY";

/// How long a wait for a transition the clock owes may take before the suite gives up.
/// Generous on purpose: the wait is on the CONDITION, so a loaded host only polls more.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The one `RegistryRecordStatus` a record is discoverable in.
const DISCOVERABLE_STATUS: &str = "APPROVED";

/// The `BatchGetDiscoverableRegistryRecordErrorCode` reported for a record the batch read
/// could not return (the enum also declares ACCESS_DENIED and INTERNAL_ERROR).
const RESOURCE_NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// The token every record's name carries, so one search query reaches each of them.
const RECORD_NAME_STEM: &str = "curation-step-probe";

/// One answer from the assembled emulator, reduced to what this suite asserts on.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    /// The `recordId`s of the answer's `registryRecords`, in response order.
    fn record_ids(&self) -> Vec<String> {
        self.body["registryRecords"]
            .as_array()
            .map(|records| {
                records
                    .iter()
                    .map(|record| record["recordId"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The summary this answer carries for `record_id`, when it carries one.
    fn summary_of(&self, record_id: &str) -> Option<Value> {
        self.body["registryRecords"]
            .as_array()?
            .iter()
            .find(|record| record["recordId"].as_str() == Some(record_id))
            .cloned()
    }
}

/// The whole emulator, exactly as the binary assembles it, over one shared engine. Request
/// authentication is off: this suite is about what a request is ANSWERED with, and the
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

/// `ApprovalConfiguration.autoApprovalRules`: the rules that decide which records are
/// approved on submission; `APPROVE_ALL` is the one rule the model declares. A registry
/// without them leaves a submitted record waiting for a curator.
async fn create_registry(router: &MultiServiceRouter, name: &str, auto_approve: bool) -> Reply {
    let mut body = json!({ "name": name, "description": format!("{name} description") });
    if auto_approve {
        body["approvalConfiguration"] = json!({ "autoApprovalRules": ["APPROVE_ALL"] });
    }
    call(router, "POST", "/registries", Some(body)).await
}

async fn get_registry(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(router, "GET", &format!("/registries/{registry_id}"), None).await
}

async fn create_record(router: &MultiServiceRouter, registry_id: &str, name: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/records"),
        Some(json!({
            "name": name,
            "description": format!("{name} description"),
            "recordType": "CUSTOM",
            "recordVersion": "1.0.0",
            "descriptors": { "custom": { "data": "{\"kind\":\"probe\"}" } },
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

async fn list_records(router: &MultiServiceRouter, registry_id: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/records-list"),
        Some(body),
    )
    .await
}

/// The records of `registry_id` whose stored `status` equals `status`:
/// `RegistryRecordFilter.values` is `@length(min: 1, max: 1)`, so one filter carries one
/// value, and `status` is one of the attributes `RegistryRecordFilterName` declares.
async fn list_records_in_status(
    router: &MultiServiceRouter,
    registry_id: &str,
    status: &str,
) -> Reply {
    list_records(
        router,
        registry_id,
        json!({
            "maxResults": 100,
            "filters": [{ "name": "status", "values": [status] }],
        }),
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

// --- the data plane's three discovery reads ------------------------------------------

async fn list_discoverable(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/registries/{registry_id}/discoverable-records-list"),
        Some(json!({})),
    )
    .await
}

async fn search_discoverable(router: &MultiServiceRouter, registry_id: &str) -> Reply {
    call(
        router,
        "POST",
        "/discoverable-records-search",
        Some(json!({ "searchQuery": RECORD_NAME_STEM, "registryIds": [registry_id] })),
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

// --- waits on a transition the clock owes --------------------------------------------

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

/// The instant a reported timestamp names, asserting its wire form on the way: every
/// timestamp member of these models targets a shape carrying `@timestampFormat("date-time")`,
/// so each is an RFC 3339 STRING and not the epoch-seconds number `restJson1` defaults to.
fn instant_of(case: &str, label: &str, value: &Value) -> f64 {
    let text = match value.as_str() {
        Some(text) => text,
        None => panic!("case {case:?}: {label} must be a date-time string, got {value:?}"),
    };
    match DateTime::from_str(text, Format::DateTime) {
        Ok(parsed) => parsed.as_secs_f64(),
        Err(error) => {
            panic!("case {case:?}: {label} must parse as a date-time, got {text:?} ({error})")
        }
    }
}

/// Assert what the DATA plane's three reads say about one record: a discoverable record is
/// reported by each of them, and one that is not is absent from the two listings and named
/// in the batch read's `errors` instead. `expected_updated_at` is the instant the control
/// plane stored for it, which a discoverable record's summary must carry too.
async fn assert_discovery_reports(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    discoverable: bool,
    expected_updated_at: &Value,
) {
    for (read, reply) in [
        ("list", list_discoverable(router, registry_id).await),
        ("search", search_discoverable(router, registry_id).await),
        (
            "batch-get",
            batch_get_discoverable(router, registry_id, record_id).await,
        ),
    ] {
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the {read} read answers 200, got {} {:?}",
            reply.status, reply.body
        );
        let reported = reply.record_ids().contains(&record_id.to_string());
        assert_eq!(
            reported, discoverable,
            "case {case:?}: the {read} read must {} the record, got {:?}",
            if discoverable {
                "report"
            } else {
                "leave out"
            },
            reply.body
        );
        if discoverable {
            let summary = reply.summary_of(record_id).unwrap_or(Value::Null);
            assert_eq!(
                &summary["updatedAt"],
                expected_updated_at,
                "case {case:?}: the {read} read reports the instant the control plane \
                 stored, got {summary:?}"
            );
        }

        // The batch read declares `errors` `@required` and names in it every id it could
        // not return, so a record that is not discoverable is reported there rather than
        // silently dropped. Asserted on the SAME answer the loop just read, so both
        // assertions are about one observation of the store.
        if read != "batch-get" {
            continue;
        }
        let errors = reply.body["errors"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            errors.len(),
            usize::from(!discoverable),
            "case {case:?}: the batch read names each id it could not return once in \
             errors, got {errors:?}"
        );
        if !discoverable {
            assert_eq!(
                (
                    errors[0]["errorCode"].as_str().unwrap_or_default(),
                    errors[0]["registryId"].as_str().unwrap_or_default(),
                    errors[0]["recordId"].as_str().unwrap_or_default(),
                ),
                (RESOURCE_NOT_FOUND_CODE, registry_id, record_id),
                "case {case:?}: the error echoes the entry's ids under \
                 {RESOURCE_NOT_FOUND_CODE}, got {errors:?}"
            );
        }
    }
}

/// One row of the table: a record staged in `start`, and the step to take from there.
struct Row {
    case: &'static str,
    start: &'static str,
    target: &'static str,
    reason: &'static str,
    registry_id: String,
    registry_arn: String,
    record_id: String,
    record_arn: String,
}

#[tokio::test]
async fn a_curation_step_is_the_change_both_planes_then_report() {
    let router = assembled_emulator();

    // (case, the status the record stands in when the step is taken, the status the step
    // moves it to, the reason carried with it). The rows span a step that makes a record
    // discoverable, two that take it off the discovery surface from different statuses, the
    // status a record already holds, and a step out of a status that was never discoverable
    // — and each carries its own reason text, so no fixed string satisfies them.
    let rows: [(&str, &str, &str, &str); 5] = [
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
            "the descriptor names no reachable endpoint",
        ),
        (
            "deprecate an approved record",
            "APPROVED",
            "DEPRECATED",
            "superseded by the next version",
        ),
        (
            "re-approve an approved record",
            "APPROVED",
            "APPROVED",
            "re-reviewed after the owner's note",
        ),
        (
            "retire a draft record",
            "DRAFT",
            "DEPRECATED",
            "never completed",
        ),
    ];

    // Two registries, so a row's start status follows from the registry's own approval
    // configuration: a submission to the auto-approving one lands in APPROVED, one to the
    // curated registry waits in PENDING_APPROVAL. Both creates are issued before either is
    // waited on, so the creation each owes the clock runs once for the pair.
    let auto = create_registry(&router, "curation-step-auto", true).await;
    let curated = create_registry(&router, "curation-step-curated", false).await;
    let mut registries = Vec::new();
    for (case, created) in [
        ("the auto-approving registry", &auto),
        ("the curated registry", &curated),
    ] {
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateRegistry answers 202, got {} {:?}",
            created.status, created.body
        );
        let arn = created.str_field("registryArn").to_string();
        registries.push((case, id_of(&arn).to_string(), arn));
    }
    for (case, registry_id, _) in &registries {
        registry_reaching(&router, case, registry_id, "READY").await;
    }
    let (_, auto_id, auto_arn) = registries[0].clone();
    let (_, curated_id, curated_arn) = registries[1].clone();

    // Every record is created before any is waited on, for the same reason.
    let mut staged: Vec<Row> = Vec::new();
    for (index, (case, start, target, reason)) in rows.into_iter().enumerate() {
        let (registry_id, registry_arn) = if start == "PENDING_APPROVAL" {
            (curated_id.clone(), curated_arn.clone())
        } else {
            (auto_id.clone(), auto_arn.clone())
        };
        let created = create_record(
            &router,
            &registry_id,
            &format!("{RECORD_NAME_STEM}-{index}"),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateRegistryRecord answers 202, got {} {:?}",
            created.status, created.body
        );
        let record_arn = created.str_field("recordArn").to_string();
        staged.push(Row {
            case,
            start,
            target,
            reason,
            registry_id,
            registry_arn,
            record_id: id_of(&record_arn).to_string(),
            record_arn,
        });
    }
    // Creation is asynchronous: each record is CREATING and the clock owes it DRAFT.
    for row in &staged {
        record_reaching(&router, row.case, &row.registry_id, &row.record_id, "DRAFT").await;
    }
    // A record reaches the workflow through a submission, which is where the registry's
    // approval configuration applies; a row that starts in DRAFT is never submitted.
    for row in &staged {
        if row.start != "DRAFT" {
            let submitted = submit_record(&router, &row.registry_id, &row.record_id).await;
            assert_eq!(
                submitted.status, 202,
                "case {:?}: SubmitRegistryRecordForApproval answers 202, got {} {:?}",
                row.case, submitted.status, submitted.body
            );
        }
    }
    for row in &staged {
        record_reaching(
            &router,
            row.case,
            &row.registry_id,
            &row.record_id,
            row.start,
        )
        .await;
    }

    for row in &staged {
        let case = row.case;
        let before = get_record(&router, &row.registry_id, &row.record_id).await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: GetRegistryRecord answers 200, got {} {:?}",
            before.status, before.body
        );
        let created_at = instant_of(case, "createdAt", &before.body["createdAt"]);
        let before_updated = instant_of(case, "updatedAt before the step", &before.body["updatedAt"]);

        // Before the step, the discovery plane reports the record exactly while the status
        // it stands in is the discoverable one.
        assert_discovery_reports(
            &router,
            &format!("{case} (before the step, in {})", row.start),
            &row.registry_id,
            &row.record_id,
            row.start == DISCOVERABLE_STATUS,
            &before.body["updatedAt"],
        )
        .await;

        let stepped = update_status(
            &router,
            &row.registry_id,
            &row.record_id,
            row.target,
            row.reason,
        )
        .await;
        assert_eq!(
            stepped.status, 202,
            "case {case:?}: UpdateRegistryRecordStatus answers 202, got {} {:?}",
            stepped.status, stepped.body
        );
        assert_eq!(
            (
                stepped.str_field("status"),
                stepped.str_field("statusReason"),
                stepped.str_field("recordId"),
                stepped.str_field("recordArn"),
                stepped.str_field("registryArn"),
            ),
            (
                row.target,
                row.reason,
                row.record_id.as_str(),
                row.record_arn.as_str(),
                row.registry_arn.as_str(),
            ),
            "case {case:?}: the answer reports the status it wrote, the reason it was given \
             and the record it addressed, got {:?}",
            stepped.body
        );

        // The instant the step answers with is the record's new last change: never before
        // the one it replaced, and never before the record was created.
        let step_updated = instant_of(case, "updatedAt on the step's answer", &stepped.body["updatedAt"]);
        assert!(
            step_updated >= before_updated && step_updated >= created_at,
            "case {case:?}: the step's updatedAt is not before the instant it replaced nor \
             before creation, got {:?} after {:?} created at {:?}",
            stepped.body["updatedAt"],
            before.body["updatedAt"],
            before.body["createdAt"]
        );

        // The write is what a later read of the owning plane reports — status, reason and
        // instant alike.
        let after = get_record(&router, &row.registry_id, &row.record_id).await;
        assert_eq!(
            (
                after.status,
                after.str_field("status"),
                after.str_field("statusReason"),
                after.body["updatedAt"].clone(),
            ),
            (
                200,
                row.target,
                row.reason,
                stepped.body["updatedAt"].clone(),
            ),
            "case {case:?}: GetRegistryRecord reports the change the step stored, got {} {:?}",
            after.status,
            after.body
        );

        // The listing reports the same stored change for the same record. `maxResults` is
        // named explicitly — the model allows 1 through 100 — so the row's record is on the
        // page whatever the service's own default page size is.
        let listed = list_records(&router, &row.registry_id, json!({ "maxResults": 100 })).await;
        let summary = listed.summary_of(&row.record_id).unwrap_or(Value::Null);
        assert_eq!(
            (
                summary["status"].clone(),
                summary["updatedAt"].clone(),
                summary["recordArn"].clone(),
            ),
            (
                json!(row.target),
                stepped.body["updatedAt"].clone(),
                json!(row.record_arn),
            ),
            "case {case:?}: the listing's summary reports the change the step stored, got \
             {summary:?} in {:?}",
            listed.body
        );

        // A listing FILTERED by status selects on the status the step wrote, and no longer
        // on the one it left.
        let under_target = list_records_in_status(&router, &row.registry_id, row.target).await;
        assert!(
            under_target.record_ids().contains(&row.record_id),
            "case {case:?}: a listing filtered to {} reports the record, got {} {:?}",
            row.target,
            under_target.status,
            under_target.body
        );
        if row.start != row.target {
            let under_start = list_records_in_status(&router, &row.registry_id, row.start).await;
            assert!(
                !under_start.record_ids().contains(&row.record_id),
                "case {case:?}: a listing filtered to {} no longer reports the record, got \
                 {} {:?}",
                row.start,
                under_start.status,
                under_start.body
            );
        }

        // And the discovery plane follows the status the step wrote, reporting the instant
        // the control plane stored for a record it can report at all.
        assert_discovery_reports(
            &router,
            &format!("{case} (after the step, in {})", row.target),
            &row.registry_id,
            &row.record_id,
            row.target == DISCOVERABLE_STATUS,
            &stepped.body["updatedAt"],
        )
        .await;
    }
}
