// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM an `importId` must be written in, and for the refusal each
//! class of value is answered with.
//!
//! The rule under test: an import id is a UUID — the form this service mints one in — and the
//! modelled `ImportId` (`@length(1..256)`, `@pattern ^[\-a-zA-Z0-9]+$`,
//! `smithy_query(query="operation",operation="DescribeImportTaskBatches")`) admits far more
//! than that. A value inside the pattern but outside that form names no import that could ever
//! exist, so it is refused as the incorrectly specified parameter it is —
//! `InvalidParameterException`, decided BEFORE any lookup — while a value written in the form
//! that no stored task carries is the missing resource it is, `ResourceNotFoundException`. The
//! two classes mean opposite things to a caller: one says rebuild the request, the other says
//! the import is not there.
//!
//! The rows span what the rule generalises over: how a value falls outside the form (a word, a
//! number, too few groups, groups of the wrong length, a non-hexadecimal character, the
//! unhyphenated spelling of a UUID, the longest value the pattern admits), the case the
//! hexadecimal is written in (the form is the groups and the hyphens, not the case), and
//! whether the operation takes the id as the resource it is about (`DescribeImportTaskBatches`)
//! or as a filter (`DescribeImportTasks`).
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it.

use serde_json::json;

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    log_group_arn, ComAmazonawsCloudwatchlogsLogs20140328State, ImportTaskRecord,
};

use crate::harness::{now_ms, Logs};

/// The two operations an `importId` is handed to that this emulator serves.
const BATCHES: &str = "DescribeImportTaskBatches";
const TASKS: &str = "DescribeImportTasks";

/// The error classes the rule chooses between, both declared by each operation
/// (`smithy_query(query="operation",operation="DescribeImportTaskBatches")`).
const INVALID_PARAMETER: &str = "InvalidParameterException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence a value outside the form is refused with: fixed text for the condition, naming
/// the member at fault and quoting nothing of the value.
const FORM_MESSAGE: &str = "ImportId must be a valid UUID format";

/// The sentence an id in the form that names no stored import is refused with.
const NOT_FOUND_MESSAGE: &str = "No matching import found.";

/// A CloudTrail Lake event data store to import from.
const SOURCE: &str =
    "arn:aws:cloudtrail:eu-west-2:123456789012:eventdatastore/22222222-2222-4222-8222-222222222222";

/// The role an import runs under.
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/CloudWatchLogsImport";

/// The id of the one import task the store holds below, written in the form CloudWatch Logs
/// gives an import task.
const STORED_ID: &str = "3b1f2d44-9c58-4a7e-8b0d-6f21c4e59a07";

/// One import task in the store, created through the constructor `CreateImportTask` itself
/// calls — so the id the cases read back is held the way a create holds one.
fn seed_task(logs: &Logs, id: &str) {
    let now = now_ms();
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let destination = log_group_arn(crate::harness::REGION, &format!("/aws/cloudtrail/{id}"), true);
    let record = ImportTaskRecord::new(id, SOURCE, ROLE_ARN, &destination, None, now);
    store.import_tasks.insert(id.to_string(), record);
}

/// The values inside the modelled pattern that are NOT the form an import id is written in.
/// Every one satisfies `^[\-a-zA-Z0-9]+$` and `@length(1..256)`, so the boundary validator
/// admits each and the handler is the one that refuses it.
fn malformed_ids() -> Vec<(&'static str, String)> {
    vec![
        ("a word", "not-a-uuid".to_string()),
        ("a single letter", "a".to_string()),
        ("a decimal number", "1234567890".to_string()),
        (
            "the unhyphenated spelling of a UUID, which this service never hands out",
            "3b1f2d449c584a7e8b0d6f21c4e59a07".to_string(),
        ),
        (
            "five groups of the wrong lengths",
            "0000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "four groups where the form has five",
            "3b1f2d44-9c58-4a7e-8b0d".to_string(),
        ),
        (
            "a group carrying a character that is not hexadecimal",
            "3b1f2d4g-9c58-4a7e-8b0d-6f21c4e59a07".to_string(),
        ),
        (
            "a UUID with one character too many in its last group",
            "3b1f2d44-9c58-4a7e-8b0d-6f21c4e59a077".to_string(),
        ),
        (
            // The longest value the member admits at all (`@length(max: 256)`).
            "the longest value the member admits",
            "a".repeat(256),
        ),
    ]
}

/// A value inside the modelled pattern but outside the form an import id is written in is
/// refused as an incorrectly specified parameter, by both operations that take one.
#[tokio::test]
async fn an_import_id_outside_the_uuid_form_is_an_invalid_parameter() {
    let logs = Logs::new();
    seed_task(&logs, STORED_ID);

    for operation in [BATCHES, TASKS] {
        for (case, id) in malformed_ids() {
            let case = format!("{operation}: {case}");
            let (code, message) = logs.refusal(operation, &json!({"importId": id})).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?}: expected {INVALID_PARAMETER} for {id:?}, got {code} \
                 {message:?}"
            );
            assert_eq!(
                message, FORM_MESSAGE,
                "case {case:?}: expected the sentence for a malformed import id, got \
                 {message:?}"
            );
        }
    }
}

/// An id written in the form that no stored task carries is the MISSING RESOURCE it is — the
/// class the form precondition is kept apart from — whichever case the hexadecimal is written
/// in.
#[tokio::test]
async fn an_import_id_in_the_form_that_names_no_task_is_a_missing_resource() {
    let logs = Logs::new();
    seed_task(&logs, STORED_ID);

    // (case, an id in the form that this account holds no import under)
    let rows: Vec<(&str, String)> = vec![
        (
            "a UUID differing from the stored id in its last character",
            "3b1f2d44-9c58-4a7e-8b0d-6f21c4e59a08".to_string(),
        ),
        (
            "a UUID bearing no resemblance to the stored id",
            "9f4c0a71-5d2e-4b18-9f6a-0c3d5e7b2a94".to_string(),
        ),
        (
            "the nil UUID",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            // The form is the five groups and the hyphens, so an id written in upper case is
            // in the form and gets as far as the lookup.
            "the stored id in upper case, which the store keys none of its tasks by",
            STORED_ID.to_uppercase(),
        ),
    ];

    for operation in [BATCHES, TASKS] {
        for (case, id) in &rows {
            let case = format!("{operation}: {case}");
            let (code, message) = logs.refusal(operation, &json!({"importId": id})).await;
            assert_eq!(
                code, NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND} for {id:?}, got {code} {message:?}"
            );
            assert_eq!(
                message, NOT_FOUND_MESSAGE,
                "case {case:?}: expected the sentence for an import the account does not \
                 hold, got {message:?}"
            );
        }
    }
}

/// The id of a task the store DOES hold is served: the precondition refuses nothing a create
/// could have minted, and both operations report that task.
#[tokio::test]
async fn the_id_of_a_stored_import_is_served_by_both_operations() {
    let logs = Logs::new();
    seed_task(&logs, STORED_ID);

    let batches = logs.ok(BATCHES, &json!({"importId": STORED_ID})).await;
    assert_eq!(
        batches["importId"].as_str(),
        Some(STORED_ID),
        "case \"the stored id, on the batches report\": expected the task's own id, got \
         {batches}"
    );
    assert!(
        batches["importBatches"].is_array(),
        "case \"the stored id, on the batches report\": expected the task's batches, got \
         {batches}"
    );

    let tasks = logs.ok(TASKS, &json!({"importId": STORED_ID})).await;
    let reported: Vec<&str> = tasks["imports"]
        .as_array()
        .unwrap_or_else(|| panic!("case \"the stored id, on the tasks report\": expected imports, got {tasks}"))
        .iter()
        .filter_map(|import| import["importId"].as_str())
        .collect();
    assert_eq!(
        reported,
        vec![STORED_ID],
        "case \"the stored id, on the tasks report\": expected the one stored task, got {tasks}"
    );
}
