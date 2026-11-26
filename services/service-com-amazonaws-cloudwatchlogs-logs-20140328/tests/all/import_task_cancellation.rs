// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CancelImportTask`: stopping an import that is still moving data,
//! and the refusals a cancel that has nothing to stop is answered with.
//!
//! The rule under test: a cancel applies to the import task the `importId` names and only
//! while that import is still under way — "cancels an ACTIVE import task" — which it then
//! leaves `CANCELLED` at the instant the cancel arrives, discarding the completion the clock
//! had armed for it and settling the batches it is grouped into with it, and the response
//! reports that task from the stored record (`importId`, `importStatus`, `importStatistics`,
//! `creationTime`, `lastUpdatedTime`). A task that has already settled is refused
//! `InvalidOperationException`, an `importId` outside the form an import id is written in is
//! refused `InvalidParameterException` before any lookup, an id in that form the account
//! holds no task under is refused `ResourceNotFoundException`, and every refusal leaves the
//! stored task exactly as it was.
//!
//! CloudWatch Logs and CloudTrail are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so every import here is started from a REAL event data store
//! CloudTrail itself holds, through `CreateImportTask` over the wire, and the cancel is
//! applied to state no part of this suite handed to CloudWatch Logs. Every case drives both
//! services the way a client does: `POST /` with the service's own `X-Amz-Target` through the
//! router that mounts them.
//!
//! The tables span what the rule generalises over rather than one request: each form an
//! `importFilter` takes (absent, one bound, both bounds) and so each number of batches an
//! import is grouped into, how many of those batches the import had already moved when the
//! cancel arrived (none, some, all), what the import had moved in bytes, how a settled task
//! came to be settled (finished on the clock, already cancelled, failed), and each class an
//! `importId` the account holds no running task under falls into — including each published
//! bound of the modelled `ImportId` length and one past it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    ComAmazonawsCloudwatchlogsLogs20140328State, IMPORT_COMPLETION_DELAY_MS,
};

/// The Region every request here is signed for.
const REGION: &str = "eu-west-2";

/// The operations under test and the reads every case answers from.
const CANCEL: &str = "CancelImportTask";
const CREATE: &str = "CreateImportTask";
const DESCRIBE: &str = "DescribeImportTasks";
const BATCHES: &str = "DescribeImportTaskBatches";

/// The modelled error codes the rule names
/// (`smithy_query(query="operation",operation="CancelImportTask")`).
const INVALID_OPERATION: &str = "InvalidOperationException";
const INVALID_PARAMETER: &str = "InvalidParameterException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The values of the model's `ImportStatus` enum (`CANCELLED | COMPLETED | FAILED |
/// IN_PROGRESS`), spelled out here rather than read from the crate so the wire spelling is
/// pinned by the test.
const IN_PROGRESS: &str = "IN_PROGRESS";
const CANCELLED: &str = "CANCELLED";
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// The whole sentence a cancel of a task that has already settled is refused with: fixed text
/// for the condition, carrying nothing of the request or of the stored task.
const NOT_ACTIVE_MESSAGE: &str = "The specified import task is not active";

/// The whole sentence an `importId` outside the form an import id is written in is refused
/// with.
const FORM_MESSAGE: &str = "ImportId must be a valid UUID format";

/// The whole sentence an id in that form that names no stored import is refused with.
const NOT_FOUND_MESSAGE: &str = "No matching import found.";

/// The published bounds of the modelled `ImportId` (`@length min 1 / max 256`), which a row of
/// ids the account holds no task under walks and then steps past.
const IMPORT_ID_MIN_LEN: usize = 1;
const IMPORT_ID_MAX_LEN: usize = 256;

/// The role an import runs under ("The ARN of the IAM role that grants CloudWatch Logs
/// permission to import from the CloudTrail Lake Event Data Store").
///
/// The role is the ACCOUNT's own — `CreateImportTask` refuses a role of another account — so
/// it is built from the emulator's account id rather than written out.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/CloudWatchLogsImport",
        ::engine::emulator_account_id()
    )
}

/// One day of event time, the span one import batch groups.
const ONE_DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Room this suite gives an import that has to be STILL RUNNING when the cancel arrives.
///
/// A created import is handed back `IN_PROGRESS` and the clock owes it a completion
/// [`IMPORT_COMPLETION_DELAY_MS`] later; the subject here is the cancel and not that span, so
/// each case pushes the instants the clock still owes the task a long way out rather than
/// racing them — the cancel then meets a running import however loaded the host is.
const RUNNING_ROOM_MS: i64 = 60 * 60 * 1_000;

/// What a FAILED import reports in `Import$errorMessage`. Nothing in this emulator fails an
/// import, so the reason a failed one would report is put in the store and read back.
const SEEDED_FAILURE: &str = "the import source became unavailable";

/// CloudWatch Logs and CloudTrail on one shared engine, reached over the wire.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope is still carried, because it is what names the Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router, engine }
    }

    /// One awsJson1_1 call to the service signing as `service` under the target `prefix`.
    async fn call(&self, service: &str, prefix: &str, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call("logs", "Logs_20140328", operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {body} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// One CloudWatch Logs call that must be refused as a client error: its wire error code
    /// and the message it reports.
    async fn logs_refusal(&self, case: &str, operation: &str, body: &Value) -> (String, String) {
        let (status, parsed) = self.call("logs", "Logs_20140328", operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} {body} must be refused with HTTP 400, got {status} \
             {parsed}"
        );
        let code = parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: an error body carries __type, got {parsed}"));
        let message = parsed
            .get("message")
            .or_else(|| parsed.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {parsed}"));
        (
            code.rsplit(['#', '/']).next().unwrap_or(code).to_string(),
            message.to_string(),
        )
    }

    /// An event data store CloudTrail itself holds, answering its ARN — the source an import
    /// reads from.
    async fn event_data_store(&self, case: &str, name: &str) -> String {
        let (status, parsed) = self
            .call(
                "cloudtrail",
                "CloudTrail_20131101",
                "CreateEventDataStore",
                &json!({"Name": name}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateEventDataStore {name} must succeed, got {status} {parsed}"
        );
        parsed["EventDataStoreArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a created event data store carries an ARN, got {parsed}")
            })
            .to_string()
    }

    /// Start one import from a store of its own, answering the created task's `importId` and
    /// the `creationTime` the create reported.
    async fn start_import(&self, case: &str, name: &str, filter: Option<Value>) -> (String, i64) {
        let source = self.event_data_store(case, name).await;
        let mut body = json!({"importSourceArn": source, "importRoleArn": role_arn()});
        if let Some(filter) = filter {
            body["importFilter"] = filter;
        }
        let answer = self.logs_ok(case, CREATE, &body).await;
        let import_id = answer["importId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateImportTask answers an importId, got {answer}"))
            .to_string();
        let creation_time = answer["creationTime"].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: CreateImportTask answers a creationTime, got {answer}")
        });
        (import_id, creation_time)
    }

    /// The one import task the service reports under `import_id`.
    async fn reported(&self, case: &str, import_id: &str) -> Value {
        let page = self
            .logs_ok(case, DESCRIBE, &json!({"importId": import_id}))
            .await;
        let imports = page["imports"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeImportTasks reports imports, got {page}"));
        assert_eq!(
            imports.len(),
            1,
            "case {case:?}: one import id reports one task, got {page}"
        );
        imports[0].clone()
    }

    /// The status the service reports for the import under `import_id`.
    async fn status_of(&self, case: &str, import_id: &str) -> String {
        let task = self.reported(case, import_id).await;
        task["importStatus"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a reported task carries a status, got {task}"))
            .to_string()
    }

    /// The `(batchId, status)` pairs the service reports for the import under `import_id`, in
    /// the order it reports them.
    async fn batch_statuses(&self, case: &str, import_id: &str) -> Vec<(String, String)> {
        let page = self
            .logs_ok(case, BATCHES, &json!({"importId": import_id}))
            .await;
        page["importBatches"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: DescribeImportTaskBatches reports importBatches, got {page}")
            })
            .iter()
            .map(|batch| {
                (
                    batch["batchId"].as_str().unwrap_or_default().to_string(),
                    batch["status"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// The ids of every import task the account holds, which a refused cancel must not change.
    async fn held_import_ids(&self, case: &str) -> Vec<String> {
        let page = self.logs_ok(case, DESCRIBE, &json!({})).await;
        page["imports"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeImportTasks reports imports, got {page}"))
            .iter()
            .map(|task| task["importId"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The ONE shared engine both services are mounted on — the store every call above reads
    /// and writes.
    ///
    /// Reached only for the few facts about an import that no request of this emulator
    /// produces: how many bytes it has moved, and the failed status nothing fails an import
    /// into. Everything asserted goes over the wire like any client's call.
    fn engine(&self) -> &Arc<engine::InMemoryEngine> {
        &self.engine
    }
}

/// Wall-clock milliseconds, the unit every CloudWatch Logs timestamp member carries.
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}

/// Give the import under `import_id` room to still be running when the cancel arrives: every
/// instant the clock still owes it is pushed [`RUNNING_ROOM_MS`] out. See that constant.
fn hold_running(emulator: &Emulator, import_id: &str) {
    let state = emulator
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(import_id)
        .unwrap_or_else(|| panic!("the created import {import_id} is stored"));
    let far = now_ms() + RUNNING_ROOM_MS;
    task.completion_due_ms = Some(far);
    for batch in &mut task.batches {
        if batch.completion_due_ms.is_some() {
            batch.completion_due_ms = Some(far);
        }
    }
}

/// Bring forward the instant the first `moved` batches of the import under `import_id` finish,
/// so the CLOCK has already moved them by the time the cancel arrives — which is how a batch
/// comes to be finished at all.
fn move_batches(emulator: &Emulator, import_id: &str, moved: usize) {
    let state = emulator
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(import_id)
        .unwrap_or_else(|| panic!("the created import {import_id} is stored"));
    let past = now_ms() - 1;
    for batch in task.batches.iter_mut().take(moved) {
        batch.completion_due_ms = Some(past);
    }
}

/// Record, for the import under `import_id`, how many bytes it had moved into the managed log
/// group — `ImportStatistics$bytesImported`, which no request of this emulator produces, so a
/// response reporting it can only be answering from the record.
fn record_bytes_imported(emulator: &Emulator, import_id: &str, bytes: i64) {
    let state = emulator
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(import_id)
        .unwrap_or_else(|| panic!("the created import {import_id} is stored"));
    task.bytes_imported = bytes;
}

/// Record that the import under `import_id` FAILED — a status the model's `ImportStatus` enum
/// carries that no request of this emulator reaches, so a cancel of a failed import can be
/// asserted over the wire at all.
fn fail_import(emulator: &Emulator, import_id: &str) {
    let state = emulator
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(import_id)
        .unwrap_or_else(|| panic!("the created import {import_id} is stored"));
    task.import_status = FAILED.to_string();
    task.last_updated_time_ms = task.creation_time_ms + 1_000;
    task.completion_due_ms = None;
    task.error_message = Some(SEEDED_FAILURE.to_string());
}

/// Bring every instant the import clock still owes into the past, the way a wait past them
/// would — without waiting for one.
///
/// A task with nothing left armed is untouched, which is exactly what a cancelled task must
/// be: the completion it was armed for was DISCARDED by the cancel, so no later read can
/// resurrect it.
fn bring_import_clocks_due(emulator: &Emulator) {
    let state = emulator
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let past = now_ms() - 1;
    for task in store.import_tasks.values_mut() {
        if task.completion_due_ms.is_some() {
            task.completion_due_ms = Some(past);
        }
        for batch in &mut task.batches {
            if batch.completion_due_ms.is_some() {
                batch.completion_due_ms = Some(past);
            }
        }
    }
}

/// A cancel of an import that is still moving data stops it: the task is `CANCELLED` from the
/// instant the cancel arrived, the response reports that task from the record, and the batches
/// the import was grouped into settle with it — the ones the clock had already moved stay
/// finished, the ones the import never got to take the status it stopped in.
#[tokio::test]
async fn a_cancel_stops_an_import_that_is_still_running() {
    let emulator = Emulator::new();
    let day = now_ms() - 30 * ONE_DAY_MS;

    // (case, the event-time window the create named, how many batches that groups the import
    // into, how many of those the clock had already moved, what the import had moved in bytes)
    let rows: Vec<(&str, Option<Value>, usize, usize, i64)> = vec![
        ("an import with no event-time window", None, 1, 0, 0),
        (
            "an import bounded at its start only",
            Some(json!({"startEventTime": day})),
            1,
            0,
            4_096,
        ),
        (
            "an import of a three-day window, one batch already moved",
            Some(json!({"startEventTime": day, "endEventTime": day + 3 * ONE_DAY_MS})),
            3,
            1,
            1_048_576,
        ),
        (
            "an import of a two-day window with every batch already moved",
            Some(json!({"startEventTime": day, "endEventTime": day + 2 * ONE_DAY_MS})),
            2,
            2,
            7,
        ),
    ];
    for (index, (case, filter, batches, moved, bytes)) in rows.into_iter().enumerate() {
        let (import_id, creation_time) = emulator
            .start_import(case, &format!("lake-store-{index}"), filter)
            .await;
        hold_running(&emulator, &import_id);
        move_batches(&emulator, &import_id, moved);
        record_bytes_imported(&emulator, &import_id, bytes);

        let before = emulator.status_of(case, &import_id).await;
        assert_eq!(
            before, IN_PROGRESS,
            "case {case:?}: expected the import to be {IN_PROGRESS} when the cancel arrives, \
             got {before}"
        );

        let opened = now_ms();
        let answer = emulator
            .logs_ok(case, CANCEL, &json!({"importId": import_id}))
            .await;
        let closed = now_ms();

        assert_eq!(
            answer["importId"].as_str(),
            Some(import_id.as_str()),
            "case {case:?}: the cancel reports the task's own id, got {answer}"
        );
        assert_eq!(
            answer["importStatus"].as_str(),
            Some(CANCELLED),
            "case {case:?}: a cancelled import is {CANCELLED}, got {answer}"
        );
        assert_eq!(
            answer["creationTime"].as_i64(),
            Some(creation_time),
            "case {case:?}: the cancel reports the instant the import was created, got {answer}"
        );
        assert_eq!(
            answer["importStatistics"]["bytesImported"].as_i64(),
            Some(bytes),
            "case {case:?}: the cancel reports what the import had moved, got {answer}"
        );
        let stopped_at = answer["lastUpdatedTime"].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: the cancel reports a lastUpdatedTime, got {answer}")
        });
        assert!(
            (opened..=closed).contains(&stopped_at) && stopped_at >= creation_time,
            "case {case:?}: expected the import to be stamped as cancelled within the call \
             ({opened}..={closed}), got {stopped_at}"
        );

        // The cancel is read back through the report: what it answered is what the store now
        // holds, not something the handler computed for the response alone.
        let task = emulator.reported(case, &import_id).await;
        assert_eq!(
            task["importStatus"].as_str(),
            Some(CANCELLED),
            "case {case:?}: the report answers {CANCELLED} after the cancel, got {task}"
        );
        assert_eq!(
            task["lastUpdatedTime"].as_i64(),
            Some(stopped_at),
            "case {case:?}: the report answers the instant the cancel stopped the import, got \
             {task}"
        );
        assert_eq!(
            task["creationTime"].as_i64(),
            Some(creation_time),
            "case {case:?}: a cancel does not move the instant the import was created, got {task}"
        );
        assert_eq!(
            task["importStatistics"]["bytesImported"].as_i64(),
            Some(bytes),
            "case {case:?}: the report answers what the import had moved, got {task}"
        );

        // "The individual batches within an import task" settle with the import: the ones the
        // clock had already finished stay finished, and the ones the import never got to take
        // the status it stopped in.
        let reported_batches = emulator.batch_statuses(case, &import_id).await;
        let expected: Vec<String> = (0..batches)
            .map(|ordinal| {
                if ordinal < moved {
                    COMPLETED.to_string()
                } else {
                    CANCELLED.to_string()
                }
            })
            .collect();
        let statuses: Vec<String> = reported_batches
            .iter()
            .map(|(_, status)| status.clone())
            .collect();
        assert_eq!(
            statuses, expected,
            "case {case:?}: expected {expected:?} for the import's batches, got \
             {reported_batches:?}"
        );
    }
}

/// The completion the clock had armed for a cancelled import is DISCARDED: once the instants
/// the clock still owes have come round, the cancelled import is still cancelled at the same
/// instant, while an import nobody cancelled is finished by that same clock.
#[tokio::test]
async fn a_cancelled_import_is_not_finished_by_the_clock_it_left() {
    let emulator = Emulator::new();

    let (stopped, _) = emulator
        .start_import("the cancelled import", "lake-store-stopped", None)
        .await;
    let (running, _) = emulator
        .start_import("the import nobody cancelled", "lake-store-running", None)
        .await;
    hold_running(&emulator, &stopped);
    hold_running(&emulator, &running);

    let answer = emulator
        .logs_ok("the cancelled import", CANCEL, &json!({"importId": stopped}))
        .await;
    let stopped_at = answer["lastUpdatedTime"]
        .as_i64()
        .unwrap_or_else(|| panic!("the cancel reports a lastUpdatedTime, got {answer}"));

    bring_import_clocks_due(&emulator);

    let cancelled = emulator.reported("the cancelled import", &stopped).await;
    assert_eq!(
        cancelled["importStatus"].as_str(),
        Some(CANCELLED),
        "case \"the cancelled import\": past the instant the create armed it is still \
         {CANCELLED}, got {cancelled}"
    );
    assert_eq!(
        cancelled["lastUpdatedTime"].as_i64(),
        Some(stopped_at),
        "case \"the cancelled import\": the instant it stopped does not move, got {cancelled}"
    );
    let batches = emulator
        .batch_statuses("the cancelled import", &stopped)
        .await;
    assert!(
        batches.iter().all(|(_, status)| status == CANCELLED),
        "case \"the cancelled import\": its batches stay {CANCELLED} too, got {batches:?}"
    );

    // The same clock, on the import nobody cancelled, does what it owed: the span the create
    // armed is what finishes an import, and only a cancel takes it away.
    assert_eq!(
        emulator
            .status_of("the import nobody cancelled", &running)
            .await,
        COMPLETED,
        "case \"the import nobody cancelled\": the clock finishes it, {IMPORT_COMPLETION_DELAY_MS}ms \
         after it was created"
    );
}

/// A cancel of an import that has already settled is refused with the fixed text of its error
/// class, whichever way it came to be settled — and the task is left exactly as it was.
#[tokio::test]
async fn a_cancel_of_a_settled_import_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();

    // Each way an import comes to be settled: finished by the clock, stopped by a cancel, and
    // failed — the one status no request of this emulator reaches.
    let (finished, _) = emulator
        .start_import("an import the clock finished", "lake-store-finished", None)
        .await;
    bring_import_clocks_due(&emulator);

    let (cancelled, _) = emulator
        .start_import("an import already cancelled", "lake-store-cancelled", None)
        .await;
    hold_running(&emulator, &cancelled);
    emulator
        .logs_ok(
            "an import already cancelled",
            CANCEL,
            &json!({"importId": cancelled}),
        )
        .await;

    let (failed, _) = emulator
        .start_import("an import whose import failed", "lake-store-failed", None)
        .await;
    fail_import(&emulator, &failed);

    // (case, the task's id, the status it is settled in)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("an import the clock finished", finished.as_str(), COMPLETED),
        ("an import already cancelled", cancelled.as_str(), CANCELLED),
        ("an import whose import failed", failed.as_str(), FAILED),
    ];
    for (case, import_id, settled_in) in rows {
        let before = emulator.reported(case, import_id).await;
        assert_eq!(
            before["importStatus"].as_str(),
            Some(settled_in),
            "case {case:?}: expected the import to be {settled_in} before the cancel, got {before}"
        );

        // Asking twice refuses identically: the refusal follows the stored status, not the
        // history of the request.
        for attempt in ["the first cancel", "the same cancel again"] {
            let (code, message) = emulator
                .logs_refusal(case, CANCEL, &json!({"importId": import_id}))
                .await;
            assert_eq!(
                code, INVALID_OPERATION,
                "case {case:?} ({attempt}): expected {INVALID_OPERATION}, got {code} {message:?}"
            );
            assert_eq!(
                message, NOT_ACTIVE_MESSAGE,
                "case {case:?} ({attempt}): expected {NOT_ACTIVE_MESSAGE:?}, got {message:?}"
            );
            for carried in [import_id, settled_in] {
                assert!(
                    !message.contains(carried),
                    "case {case:?} ({attempt}): the refusal carries nothing of the request or \
                     the stored task, got {carried:?} in {message:?}"
                );
            }
        }

        let after = emulator.reported(case, import_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused cancel leaves the task untouched, got {after}"
        );
    }
}

/// An `importId` outside the form an import id is written in names no import that could ever
/// exist, so it is refused as the incorrectly specified parameter it is — before any lookup —
/// while an id in that form the account holds no task under is the missing resource it is.
/// Either way nothing is cancelled.
#[tokio::test]
async fn an_id_that_names_no_running_import_is_refused_as_the_class_it_is() {
    let emulator = Emulator::new();
    // One running import, so the refusals below are a lookup against the store rather than a
    // handler that refuses every cancel.
    let (live, _) = emulator
        .start_import("the live import", "lake-store-live", None)
        .await;
    hold_running(&emulator, &live);
    let held = emulator.held_import_ids("the live import").await;

    // (case, the id the cancel names, the code it is refused under, the sentence it carries —
    // `None` where the refusal is the boundary validator's own wording). Every value is inside
    // the modelled `ImportId` pattern (`^[\-a-zA-Z0-9]+$`) except as a row says.
    let rows: Vec<(String, String, &str, Option<&str>)> = vec![
        (
            "a word, which is in no form an import id is written in".to_string(),
            "not-a-uuid".to_string(),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            "the unhyphenated spelling of a UUID, which this service never hands out".to_string(),
            "3b1f2d449c584a7e8b0d6f21c4e59a07".to_string(),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            "four groups where the form has five".to_string(),
            "3b1f2d44-9c58-4a7e-8b0d".to_string(),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            "a group carrying a character that is not hexadecimal".to_string(),
            "3b1f2d4g-9c58-4a7e-8b0d-6f21c4e59a07".to_string(),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            format!("an id of the modelled minimum {IMPORT_ID_MIN_LEN} character"),
            "x".repeat(IMPORT_ID_MIN_LEN),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            format!("an id of the modelled maximum {IMPORT_ID_MAX_LEN} characters"),
            "x".repeat(IMPORT_ID_MAX_LEN),
            INVALID_PARAMETER,
            Some(FORM_MESSAGE),
        ),
        (
            format!("an id one character past the modelled maximum of {IMPORT_ID_MAX_LEN}"),
            "x".repeat(IMPORT_ID_MAX_LEN + 1),
            INVALID_PARAMETER,
            None,
        ),
        (
            "a UUID the account holds no import under".to_string(),
            "9f4c0a71-5d2e-4b18-9f6a-0c3d5e7b2a94".to_string(),
            NOT_FOUND,
            Some(NOT_FOUND_MESSAGE),
        ),
        (
            "the nil UUID".to_string(),
            "00000000-0000-0000-0000-000000000000".to_string(),
            NOT_FOUND,
            Some(NOT_FOUND_MESSAGE),
        ),
        (
            // The form is the five groups and the hyphens, so an id written in upper case is
            // in the form and gets as far as the lookup.
            "a UUID in upper case, which the store keys no task by".to_string(),
            "3B1F2D44-9C58-4A7E-8B0D-6F21C4E59A07".to_string(),
            NOT_FOUND,
            Some(NOT_FOUND_MESSAGE),
        ),
    ];
    for (case, import_id, expected_code, expected_message) in rows {
        let (code, message) = emulator
            .logs_refusal(&case, CANCEL, &json!({"importId": import_id}))
            .await;
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code} for {import_id:?}, got {code} {message:?}"
        );
        if let Some(expected) = expected_message {
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
        assert_eq!(
            emulator.held_import_ids(&case).await,
            held,
            "case {case:?}: a refused cancel leaves the account's imports as they were"
        );
    }

    // Positive control: the import the store really holds is cancellable on the same request
    // the ids above were refused on.
    emulator
        .logs_ok("the live import", CANCEL, &json!({"importId": live}))
        .await;
    assert_eq!(
        emulator.status_of("the live import", &live).await,
        CANCELLED,
        "case \"the live import\": the cancel the unknown ids were refused for is accepted"
    );
}
