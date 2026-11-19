// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeImportTaskBatches`: what the service reports about "the
//! individual batches within an import task".
//!
//! The rule under test: an import task's events are grouped into batches by EVENT TIME — one
//! batch per day of the event-time window the create bounded at both ends, a single batch when
//! it left either end open, and never more than the 50 a task is grouped into at most — and
//! `DescribeImportTaskBatches` reports those batches of the task its `importId` names, from
//! stored state: each one carrying the model's `ImportBatch` members as it holds them, in one
//! stable order, narrowed to the statuses `batchImportStatus` names, handed back at most `limit`
//! (default 10) per page with a `nextToken` exactly when more remain, beside the task's own
//! `importId` and `importSourceArn`. A batch's status is the one the CLOCK has already made due
//! — a read never moves one on — and an `importId` no task carries is a missing resource rather
//! than an empty page.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.DescribeImportTaskBatches` through the router that mounts it — so
//! what is asserted here is what a client reads off the wire.
//!
//! The grouping table CREATES its import tasks over the wire with `CreateImportTask`, so the
//! batches read back are the ones a real create produced. Every case about the statuses the
//! clock owes seeds its task through [`ImportTaskRecord::new`] — the SAME constructor that
//! create calls — and then states each batch's completion instant, so a case asserts exact
//! statuses instead of racing the host.
//!
//! The table spans what the rule generalises over rather than one request: a window open at
//! neither / one / both ends, windows from zero length to past the batch ceiling, each value of
//! the model's `ImportStatus` enum as a filter and in combination, a task stopped mid-import in
//! each of the statuses it can stop in, every page size from one batch to more than the task
//! holds, and both published bounds of `limit` with one past each.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::ops::types::ImportFilter;
use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    log_group_arn, ComAmazonawsCloudwatchlogsLogs20140328State, ImportTaskRecord,
    IMPORT_STATUS_CANCELLED, IMPORT_STATUS_COMPLETED, IMPORT_STATUS_FAILED,
    IMPORT_STATUS_IN_PROGRESS,
};

use crate::harness::{now_ms, Logs};

/// The operation every case calls.
const OP: &str = "DescribeImportTaskBatches";

/// A CloudTrail Lake event data store to import from — "the ARN of the source to import from".
const SOURCE: &str =
    "arn:aws:cloudtrail:eu-west-2:123456789012:eventdatastore/11111111-1111-1111-1111-111111111111";

/// The role an import runs under ("The ARN of the IAM role that grants CloudWatch Logs
/// permission to import from the CloudTrail Lake Event Data Store").
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CloudWatchLogsImport";

/// One day of event time, which is the span one batch of an import groups: a batch is "a subset
/// of stored events grouped by their eventTime" (`DescribeImportTaskBatches`).
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// The most batches one import task is grouped into, however long its event-time window is.
const MAX_BATCHES: usize = 50;

/// The number of batches one page carries when the request omits `limit`: "The maximum number of
/// import batches to return in the response. Default: 10".
const DEFAULT_LIMIT: usize = 10;

/// The longest `batchId` the model admits: `BatchId` carries `@length(min: 1, max: 256)`.
const MAX_BATCH_ID_LEN: usize = 256;

/// A span far longer than any test's run, for a completion instant a case wants to stay ahead of
/// the clock.
const FAR_AHEAD_MS: i64 = 60 * 60 * 1_000;

/// What a FAILED import reports in `Import$errorMessage`, which is also what a batch that failed
/// with it reports in `ImportBatch$errorMessage` ("Only present when status is FAILED"). Any text
/// does: the rule under test is that the member is reported from state for a failed batch and
/// absent for every other.
const FAILURE: &str = "the import source became unavailable";

/// The managed destination log group's ARN for one import: the resource `CreateImportTask`
/// documents creating, "arn:aws:logs:…:log-group:/aws/cloudtrail/…".
fn destination_arn(id: &str) -> String {
    log_group_arn(crate::harness::REGION, &format!("/aws/cloudtrail/{id}"), true)
}

/// One import task in the store, created through the constructor `CreateImportTask` itself calls,
/// with every batch's completion instant pushed past the end of the test run — so the statuses a
/// case reads are the ones it then states rather than whatever the host's clock reached.
///
/// Answers how many batches the task was grouped into.
fn seed_task(logs: &Logs, id: &str, filter: Option<ImportFilter>) -> usize {
    let now = now_ms();
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let mut record = ImportTaskRecord::new(
        id,
        SOURCE,
        ROLE_ARN,
        &destination_arn(id),
        filter.as_ref(),
        now,
    );
    record.completion_due_ms = Some(now + FAR_AHEAD_MS);
    for batch in &mut record.batches {
        batch.completion_due_ms = Some(now + FAR_AHEAD_MS);
    }
    let count = record.batches.len();
    store.import_tasks.insert(id.to_string(), record);
    count
}

/// Bring the first `completed` batches of the stored task `id` due, by moving their completion
/// instants into the past — the clock-owed transition a report then materialises.
fn complete_batches(logs: &Logs, id: &str, completed: usize) {
    let now = now_ms();
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(id)
        .unwrap_or_else(|| panic!("the task {id} is stored"));
    for batch in task.batches.iter_mut().take(completed) {
        batch.completion_due_ms = Some(now - 1);
    }
}

/// Stop the stored task `id` in `status` — the status it reached when it stopped moving data,
/// `lastUpdatedTime` ms ago — carrying `error_message` where a failed import reports one.
fn stop_task(logs: &Logs, id: &str, status: &str, error_message: Option<&str>) {
    let now = now_ms();
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let task = store
        .import_tasks
        .get_mut(id)
        .unwrap_or_else(|| panic!("the task {id} is stored"));
    task.import_status = status.to_string();
    task.completion_due_ms = None;
    task.last_updated_time_ms = now - 1;
    task.error_message = error_message.map(ToString::to_string);
}

/// The `importBatches` member of one successful page.
async fn page(logs: &Logs, body: &Value) -> Vec<Value> {
    let response = logs.ok(OP, body).await;
    response
        .get("importBatches")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("{OP} {body}: the response must carry importBatches, got {response}")
        })
        .clone()
}

/// A string member of one reported batch.
fn member(case: &str, batch: &Value, name: &str) -> String {
    batch
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: every batch carries {name}, got {batch}"))
        .to_string()
}

/// Every batch of one enumeration, walked page by page until the service reports no more.
async fn every_batch(logs: &Logs, case: &str, request: &Value) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=MAX_BATCHES {
        let mut body = request.clone();
        if let Some(next) = &token {
            body["nextToken"] = json!(next);
        }
        let response = logs.ok(OP, &body).await;
        let batches = response
            .get("importBatches")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the response must carry importBatches, got {response}")
            })
            .clone();
        collected.extend(batches);
        token = response
            .get("nextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {case:?}: paging the batches of one task must terminate")
}

/// The `batchId`s of a reported list, in the order they came back.
fn ids(case: &str, batches: &[Value]) -> Vec<String> {
    batches
        .iter()
        .map(|batch| member(case, batch, "batchId"))
        .collect()
}

/// One grouping case: the `importFilter` a create sends, and the number of batches the import is
/// then grouped into.
struct Grouping {
    case: &'static str,
    /// The `importFilter` member, or `None` for a create that sends none.
    filter: Option<Value>,
    batches: usize,
}

/// The windows the grouping rule generalises over: open at neither end, open at one end, and
/// bounded at both from zero length to well past the ceiling on a task's batches.
fn groupings() -> Vec<Grouping> {
    let start = 1_600_000_000_000_i64;
    vec![
        Grouping {
            case: "no event-time filter at all",
            filter: None,
            batches: 1,
        },
        Grouping {
            case: "a start bound only",
            filter: Some(json!({"startEventTime": start})),
            batches: 1,
        },
        Grouping {
            case: "an end bound only",
            filter: Some(json!({"endEventTime": start + 7 * DAY_MS})),
            batches: 1,
        },
        Grouping {
            case: "both bounds at the same instant",
            filter: Some(json!({"startEventTime": start, "endEventTime": start})),
            batches: 1,
        },
        Grouping {
            case: "a window of one millisecond",
            filter: Some(json!({"startEventTime": start, "endEventTime": start + 1})),
            batches: 1,
        },
        Grouping {
            case: "a window of exactly one day",
            filter: Some(json!({"startEventTime": start, "endEventTime": start + DAY_MS})),
            batches: 1,
        },
        Grouping {
            case: "a window one millisecond past one day",
            filter: Some(json!({"startEventTime": start, "endEventTime": start + DAY_MS + 1})),
            batches: 2,
        },
        Grouping {
            case: "a window of three days",
            filter: Some(json!({"startEventTime": start, "endEventTime": start + 3 * DAY_MS})),
            batches: 3,
        },
        Grouping {
            case: "a window of twelve days",
            filter: Some(json!({"startEventTime": start, "endEventTime": start + 12 * DAY_MS})),
            batches: 12,
        },
        Grouping {
            case: "a window of exactly the batch ceiling in days",
            filter: Some(json!({
                "startEventTime": start,
                "endEventTime": start + 50 * DAY_MS,
            })),
            batches: MAX_BATCHES,
        },
        Grouping {
            case: "a window of a year, past the batch ceiling",
            filter: Some(json!({
                "startEventTime": start,
                "endEventTime": start + 365 * DAY_MS,
            })),
            batches: MAX_BATCHES,
        },
    ]
}

/// An import CREATED over the wire is grouped into the batches its event-time window implies,
/// and every one of them is reported back under the created task's own id and source.
#[tokio::test]
async fn a_created_import_reports_the_batches_its_window_implies() {
    for grouping in groupings() {
        let case = grouping.case;
        // One emulator per row, so the account's three active-import slots do not couple the
        // rows of this table.
        let logs = Logs::new();

        let mut request = json!({"importSourceArn": SOURCE, "importRoleArn": ROLE_ARN});
        if let Some(filter) = grouping.filter.clone() {
            request["importFilter"] = filter;
        }
        let created = logs.ok("CreateImportTask", &request).await;
        let import_id = created
            .get("importId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a created import carries an importId, got {created}"))
            .to_string();

        let response = logs
            .ok(OP, &json!({"importId": import_id, "limit": MAX_BATCHES}))
            .await;
        assert_eq!(
            response.get("importId").and_then(Value::as_str),
            Some(import_id.as_str()),
            "case {case:?}: the response names the task it reports on, got {response}"
        );
        assert_eq!(
            response.get("importSourceArn").and_then(Value::as_str),
            Some(SOURCE),
            "case {case:?}: the response names the source being imported from, got {response}"
        );
        let batches = response
            .get("importBatches")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected importBatches, got {response}"))
            .clone();
        assert_eq!(
            batches.len(),
            grouping.batches,
            "case {case:?}: expected {} batch(es), got {}",
            grouping.batches,
            batches.len()
        );

        let reported = ids(case, &batches);
        let mut unique = reported.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            reported.len(),
            "case {case:?}: every batchId is unique within the task, got {reported:?}"
        );
        for id in &reported {
            assert!(
                !id.is_empty() && id.len() <= MAX_BATCH_ID_LEN,
                "case {case:?}: a batchId is 1..{MAX_BATCH_ID_LEN} characters, got {id:?}"
            );
        }
        // A batch of a running import is in one of the two statuses an import that has not
        // stopped can be in, and only a FAILED batch reports an error message.
        for batch in &batches {
            let status = member(case, batch, "status");
            assert!(
                status == IMPORT_STATUS_IN_PROGRESS || status == IMPORT_STATUS_COMPLETED,
                "case {case:?}: a batch of a running import is IN_PROGRESS or COMPLETED, \
                 got {status:?}"
            );
            assert!(
                batch.get("errorMessage").is_none(),
                "case {case:?}: only a FAILED batch reports an errorMessage, got {batch}"
            );
        }
    }
}

/// The status a batch is reported in is the one the CLOCK already made due: a batch whose
/// completion instant has passed is `COMPLETED` and one whose instant is still ahead stays
/// `IN_PROGRESS` — and reporting twice reports the same thing, because a read moves nothing on.
#[tokio::test]
async fn the_completions_the_clock_owes_are_reported_and_reading_changes_nothing() {
    let logs = Logs::new();
    let id = "import-clock";
    // Four batches: a window of four days.
    let count = seed_task(
        &logs,
        id,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 4 * DAY_MS),
        }),
    );
    assert_eq!(count, 4, "a window of four days is four batches, got {count}");
    complete_batches(&logs, id, 2);

    let expected = [
        IMPORT_STATUS_COMPLETED,
        IMPORT_STATUS_COMPLETED,
        IMPORT_STATUS_IN_PROGRESS,
        IMPORT_STATUS_IN_PROGRESS,
    ];
    // Twice, so a read that changed what a later caller sees would show up here.
    for round in 1..=2 {
        let case = format!("round {round}");
        let batches = page(&logs, &json!({"importId": id})).await;
        let statuses: Vec<String> = batches
            .iter()
            .map(|batch| member(&case, batch, "status"))
            .collect();
        assert_eq!(
            statuses, expected,
            "case {case:?}: expected the statuses the clock made due {expected:?}, got {statuses:?}"
        );
    }

    // And the task itself is still running: reporting its batches moved it nowhere.
    let imports = logs
        .ok("DescribeImportTasks", &json!({"importId": id}))
        .await;
    assert_eq!(
        imports["imports"][0]
            .get("importStatus")
            .and_then(Value::as_str),
        Some(IMPORT_STATUS_IN_PROGRESS),
        "reporting an import's batches must not move the import on, got {imports}"
    );
}

/// An import that STOPPED mid-flight reports its batches as they ended: the ones the import had
/// already finished stay `COMPLETED`, and the ones it never got to carry the status the task
/// stopped in — with the import's error message on the failed ones alone.
#[tokio::test]
async fn a_stopped_import_reports_the_status_each_batch_ended_in() {
    // (case, the status the task stopped in, the error message it carries)
    let rows: [(&str, &str, Option<&str>); 2] = [
        ("an import a caller cancelled", IMPORT_STATUS_CANCELLED, None),
        (
            "an import that stopped on an error",
            IMPORT_STATUS_FAILED,
            Some(FAILURE),
        ),
    ];

    for (case, status, failure) in rows {
        let logs = Logs::new();
        let id = "import-stopped";
        let count = seed_task(
            &logs,
            id,
            Some(ImportFilter {
                start_event_time: Some(1_500_000_000_000),
                end_event_time: Some(1_500_000_000_000 + 3 * DAY_MS),
            }),
        );
        assert_eq!(
            count, 3,
            "case {case:?}: a window of three days is three batches, got {count}"
        );
        // The first batch had finished before the import stopped; the other two had not.
        complete_batches(&logs, id, 1);
        stop_task(&logs, id, status, failure);

        let batches = page(&logs, &json!({"importId": id})).await;
        let statuses: Vec<String> = batches
            .iter()
            .map(|batch| member(case, batch, "status"))
            .collect();
        let expected = [
            IMPORT_STATUS_COMPLETED.to_string(),
            status.to_string(),
            status.to_string(),
        ];
        assert_eq!(
            statuses, expected,
            "case {case:?}: expected {expected:?}, got {statuses:?}"
        );
        for batch in &batches {
            let reported = batch.get("errorMessage").and_then(Value::as_str);
            let owed = match member(case, batch, "status").as_str() {
                IMPORT_STATUS_FAILED => failure,
                _ => None,
            };
            assert_eq!(
                reported, owed,
                "case {case:?}: an errorMessage is present only on a FAILED batch, \
                 expected {owed:?}, got {batch}"
            );
        }
    }
}

/// `batchImportStatus` selects exactly the batches in the statuses it names — one value, several,
/// a value no batch of the task is in, every value of the model's `ImportStatus` enum, and none
/// at all, which excludes nothing.
#[tokio::test]
async fn the_status_filter_selects_exactly_the_batches_in_those_statuses() {
    let logs = Logs::new();
    let id = "import-filtered";
    let count = seed_task(
        &logs,
        id,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 5 * DAY_MS),
        }),
    );
    assert_eq!(count, 5, "a window of five days is five batches, got {count}");
    // Two of the five have finished; the other three are still running.
    complete_batches(&logs, id, 2);
    let all = [
        format!("{id}-1"),
        format!("{id}-2"),
        format!("{id}-3"),
        format!("{id}-4"),
        format!("{id}-5"),
    ];

    // (case, the batchImportStatus the request names, the batch ordinals the rule requires)
    let rows: Vec<(&str, Value, Vec<usize>)> = vec![
        ("no status filter at all", json!({}), vec![1, 2, 3, 4, 5]),
        (
            "COMPLETED alone",
            json!({"batchImportStatus": [IMPORT_STATUS_COMPLETED]}),
            vec![1, 2],
        ),
        (
            "IN_PROGRESS alone",
            json!({"batchImportStatus": [IMPORT_STATUS_IN_PROGRESS]}),
            vec![3, 4, 5],
        ),
        (
            "CANCELLED, which no batch of this task is in",
            json!({"batchImportStatus": [IMPORT_STATUS_CANCELLED]}),
            vec![],
        ),
        (
            "FAILED, which no batch of this task is in",
            json!({"batchImportStatus": [IMPORT_STATUS_FAILED]}),
            vec![],
        ),
        (
            "two statuses, one of which no batch is in",
            json!({"batchImportStatus": [IMPORT_STATUS_COMPLETED, IMPORT_STATUS_FAILED]}),
            vec![1, 2],
        ),
        (
            "both statuses the task's batches are in",
            json!({"batchImportStatus": [IMPORT_STATUS_IN_PROGRESS, IMPORT_STATUS_COMPLETED]}),
            vec![1, 2, 3, 4, 5],
        ),
        (
            "every value of the ImportStatus enum",
            json!({"batchImportStatus": [
                IMPORT_STATUS_IN_PROGRESS,
                IMPORT_STATUS_CANCELLED,
                IMPORT_STATUS_COMPLETED,
                IMPORT_STATUS_FAILED,
            ]}),
            vec![1, 2, 3, 4, 5],
        ),
        (
            "a filter that names no status at all",
            json!({"batchImportStatus": []}),
            vec![1, 2, 3, 4, 5],
        ),
    ];

    for (case, filter, ordinals) in rows {
        let mut body = filter;
        body["importId"] = json!(id);
        let expected: Vec<String> = ordinals
            .iter()
            .map(|ordinal| all[ordinal - 1].clone())
            .collect();
        let reported = ids(case, &every_batch(&logs, case, &body).await);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the batches {expected:?}, got {reported:?}"
        );
    }
}

/// Whatever the page size, paging walks the one enumeration exactly once: every page is at most
/// `limit` long, a `nextToken` comes back exactly when batches remain, and the pages concatenate
/// to the whole enumeration with nothing repeated or skipped. A request that omits `limit` gets
/// the documented default.
#[tokio::test]
async fn paging_walks_the_batches_once_at_every_page_size() {
    let logs = Logs::new();
    let id = "import-paged";
    let count = seed_task(
        &logs,
        id,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 12 * DAY_MS),
        }),
    );
    assert_eq!(
        count, 12,
        "a window of twelve days is twelve batches, got {count}"
    );
    let expected: Vec<String> = (1..=12).map(|ordinal| format!("{id}-{ordinal}")).collect();

    // One page size below the task's batch count, one at it, one above, and both published
    // bounds of the modelled `limit`.
    for limit in [1_usize, 2, 5, 11, 12, 13, MAX_BATCHES] {
        let case = format!("limit {limit}");
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let body = match &token {
                Some(next) => json!({"importId": id, "limit": limit, "nextToken": next}),
                None => json!({"importId": id, "limit": limit}),
            };
            let response = logs.ok(OP, &body).await;
            let batches = response
                .get("importBatches")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {case:?}: expected importBatches, got {response}"))
                .clone();
            assert!(
                batches.len() <= limit,
                "case {case:?}: a page holds at most {limit} batch(es), got {}",
                batches.len()
            );
            collected.extend(ids(&case, &batches));
            token = response
                .get("nextToken")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let remaining = expected.len() - collected.len();
            assert_eq!(
                token.is_some(),
                remaining > 0,
                "case {case:?}: after {} of {} batches the nextToken presence must match \
                 whether more remain, got {response}",
                collected.len(),
                expected.len()
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            collected, expected,
            "case {case:?}: the pages must concatenate to the whole enumeration, expected \
             {expected:?}, got {collected:?}"
        );
    }

    // "Default: 10": a request that names no limit is answered the first ten batches, and its
    // token walks on to the rest.
    let first = logs.ok(OP, &json!({"importId": id})).await;
    let page_ids = ids("the default limit", &page(&logs, &json!({"importId": id})).await);
    assert_eq!(
        page_ids.as_slice(),
        &expected[..DEFAULT_LIMIT],
        "case \"the default limit\": expected the first {DEFAULT_LIMIT} batches, got {page_ids:?}"
    );
    let token = first
        .get("nextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("twelve batches at the default page size leave more, got {first}"))
        .to_string();
    let rest = ids(
        "the default limit, continued",
        &page(&logs, &json!({"importId": id, "nextToken": token})).await,
    );
    assert_eq!(
        rest.as_slice(),
        &expected[DEFAULT_LIMIT..],
        "case \"the default limit, continued\": expected the remaining batches, got {rest:?}"
    );
}

/// An `importId` no task carries names a resource that does not exist: the operation refuses with
/// `ResourceNotFoundException` and the sentence CloudWatch Logs words that condition in, rather
/// than reporting an empty list of batches.
#[tokio::test]
async fn an_import_id_no_task_carries_is_a_missing_resource() {
    let logs = Logs::new();
    seed_task(&logs, "import-present", None);

    // Ids no stored task carries: one of the same shape as the stored id, one that is the stored
    // id with a suffix, and one bearing no resemblance.
    for id in ["import-absent", "import-present-gone", "NOTHING"] {
        let (code, message) = logs.refusal(OP, &json!({"importId": id})).await;
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {id:?}: expected ResourceNotFoundException, got {code} {message:?}"
        );
        assert_eq!(
            message, "No matching import found.",
            "case {id:?}: expected the sentence CloudWatch Logs states for an id it holds no \
             import under, got {message:?}"
        );
    }
}

/// A `nextToken` is a position within ONE task's filtered enumeration: continuing a different one
/// with it, or handing over something this service never minted, is refused rather than answered
/// from the wrong place.
#[tokio::test]
async fn a_token_from_another_enumeration_is_refused() {
    let logs = Logs::new();
    let id = "import-tokens";
    let other = "import-other";
    seed_task(
        &logs,
        id,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 3 * DAY_MS),
        }),
    );
    seed_task(
        &logs,
        other,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 3 * DAY_MS),
        }),
    );

    let first = logs.ok(OP, &json!({"importId": id, "limit": 1})).await;
    let token = first
        .get("nextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the first page of three batches carries a nextToken, got {first}"))
        .to_string();

    let rows: Vec<(&str, Value)> = vec![
        (
            "a token from the unfiltered enumeration, replayed with a status filter",
            json!({
                "importId": id,
                "limit": 1,
                "nextToken": token,
                "batchImportStatus": [IMPORT_STATUS_IN_PROGRESS],
            }),
        ),
        (
            "a token from another task's enumeration",
            json!({"importId": other, "limit": 1, "nextToken": token}),
        ),
        (
            "a token that is not hex at all",
            json!({"importId": id, "nextToken": "not-a-token"}),
        ),
        (
            "a token that is hex but carries no cursor",
            json!({"importId": id, "nextToken": "0011deadbeef"}),
        ),
    ];
    for (case, body) in rows {
        let code = logs.err(OP, &body).await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {code}"
        );
    }

    // The token itself still works for the enumeration it was minted for.
    let continued = ids(
        "the token's own enumeration",
        &page(&logs, &json!({"importId": id, "limit": 1, "nextToken": token})).await,
    );
    assert_eq!(
        continued,
        [format!("{id}-2")],
        "case \"the token's own enumeration\": expected the second batch, got {continued:?}"
    );
}

/// Both published bounds of `limit` reach the operation, and one past either is refused by the
/// modelled constraint. Source: the model's `DescribeLimit` shape, `@range` 1..50, which
/// `DescribeImportTaskBatches$limit` targets.
#[tokio::test]
async fn the_published_limit_bounds_are_accepted_and_one_past_each_is_refused() {
    let logs = Logs::new();
    let id = "import-limits";
    seed_task(
        &logs,
        id,
        Some(ImportFilter {
            start_event_time: Some(1_600_000_000_000),
            end_event_time: Some(1_600_000_000_000 + 3 * DAY_MS),
        }),
    );

    for limit in [1_usize, MAX_BATCHES] {
        let case = format!("limit {limit}");
        let reported = ids(&case, &page(&logs, &json!({"importId": id, "limit": limit})).await);
        let expected: Vec<String> = (1..=3)
            .map(|ordinal| format!("{id}-{ordinal}"))
            .take(limit)
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: a value on the published bound is answered, expected {expected:?}, \
             got {reported:?}"
        );
    }

    // (limit, the constraint clause the refusal must state)
    let rows: [(i64, &str); 2] = [
        (0, "Member must have value greater than or equal to 1"),
        (51, "Member must have value less than or equal to 50"),
    ];
    for (limit, clause) in rows {
        let (code, message) = logs
            .refusal(OP, &json!({"importId": id, "limit": limit}))
            .await;
        assert_eq!(
            code, "InvalidParameterException",
            "case \"limit {limit}\": expected InvalidParameterException, got {code} {message:?}"
        );
        let expected = format!(
            "1 validation error detected: Value at 'limit' failed to satisfy constraint: {clause}"
        );
        assert_eq!(
            message, expected,
            "case \"limit {limit}\": expected {expected:?}, got {message:?}"
        );
    }
}
