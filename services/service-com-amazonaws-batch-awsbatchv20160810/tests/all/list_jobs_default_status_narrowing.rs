// SPDX-License-Identifier: CC0-1.0

//! WHAT a `ListJobs` that names no `jobStatus` narrows to, for every selector it can name.
//!
//! The rule under test: a `ListJobs` with no `filters` narrows the jobs its selector chose to
//! exactly the ones in ONE `JobStatus` — the status the request names, or `RUNNING` when it
//! names none — and which of `jobQueue`, `arrayJobId` and `multiNodeJobId` chose them makes
//! no difference to that narrowing. `ListJobsRequest$jobStatus` is "the job status used to
//! filter jobs in the specified queue … If you don't specify a status, only `RUNNING` jobs are
//! returned", and the same member tells a caller listing an array to ASK for the status it
//! wants ("Array job parents are updated to PENDING when any child job is updated to RUNNABLE
//! and remain in PENDING status while child jobs are running. To view these jobs, filter by
//! PENDING status until all child jobs reach a terminal state") — advice a caller needs only
//! because the default narrows an array listing too. Source:
//! `smithy_query(query="doc", shape="ListJobsRequest", member="jobStatus")`.
//!
//! The default is a `RUNNING` FILTER and not an empty answer: the same unfiltered listing that
//! hides a child in any other status reports one that IS `RUNNING`. Both halves are asserted,
//! because an implementation that answered nothing at all for an array listing would satisfy
//! only one of them.
//!
//! The rows differ along what the rule generalises over: which selector the listing names,
//! whether it names a status, WHICH of the modelled statuses it names, which status each
//! selected job is in, and how many jobs are in it.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{
    compute_environment, container_properties, describe_job, job_definition, job_queue, Batch,
};

/// Every modelled `JobStatus`, spelled out rather than imported so a wrong constant in the
/// implementation cannot make this suite pass. Source:
/// `smithy_query(query="shape", shape="JobStatus")`, which lists exactly these members.
const JOB_STATUSES: [&str; 7] = [
    "SUBMITTED",
    "PENDING",
    "RUNNABLE",
    "STARTING",
    "RUNNING",
    "SUCCEEDED",
    "FAILED",
];

/// The status a listing that names none narrows to ("If you don't specify a status, only
/// RUNNING jobs are returned").
const RUNNING: &str = "RUNNING";

/// The status an array PARENT holds while any child of it is still live, which is what the
/// queue listings below see the parent under.
const PENDING: &str = "PENDING";

/// How many times [`until_the_clock_owes_nothing`] looks before it gives up.
///
/// Each clock-owed move falls due `state::JOB_PHASE_MILLIS` after the one before it and a job
/// submitted to a queue with no capacity makes one before it rests, so this is many phases of
/// headroom on a slow machine.
const SETTLE_POLLS: usize = 240;

/// The job summaries `ListJobs` answers for `request`.
async fn listed(batch: &Batch, case: &str, request: Value) -> Vec<Value> {
    let (status, body) = batch.call("/v1/listjobs", request).await;
    assert_eq!(
        status, 200,
        "case {case}: expected HTTP 200 from ListJobs, got {status} with {body}"
    );
    body["jobSummaryList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `jobId`s of a listing, in ascending order, so a row states WHICH jobs it expects
/// without depending on the order the page happens to answer them in.
fn ids(listed: &[Value]) -> Vec<String> {
    let mut found: Vec<String> = listed
        .iter()
        .map(|summary| summary["jobId"].as_str().unwrap_or("<no jobId>").to_string())
        .collect();
    found.sort_unstable();
    found
}

/// Drive this store's clock, through reads, until it owes no job transition at all.
///
/// The statuses the tests below PIN would otherwise be overwritten by a move that was still
/// due. Polls for that condition rather than waiting a fixed time, so it holds on a loaded
/// machine as well as an idle one.
async fn until_the_clock_owes_nothing(batch: &Batch, case: &str, job_id: &str) {
    let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
    for _ in 0..SETTLE_POLLS {
        // A read is what materialises whatever has fallen due, so the loop looks through one.
        describe_job(batch, job_id).await;
        let owed = batch.store().lock().next_job_due_at();
        if owed.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis((span / 10).max(5))).await;
    }
    panic!("case {case}: the store still owes a job transition, so no status can be pinned");
}

/// Put each child of `parent_id` in the status `statuses` gives for its index, as of `at`, and
/// clear what the clock owed it — the children rest in their queue owing nothing, so this is
/// the whole of what could move them.
fn pin_children(batch: &Batch, case: &str, parent_id: &str, statuses: &[&str], at: i64) {
    let store = batch.store();
    let mut store = store.lock();
    for (index, status) in statuses.iter().enumerate() {
        let child_id = state::array_child_job_id(parent_id, i32::try_from(index).unwrap_or(0));
        let child = store.jobs.get_mut(&child_id).unwrap_or_else(|| {
            panic!("case {case}: the submit must have stored child {index} as {child_id}")
        });
        child.status = (*status).to_string();
        child.status_since = at;
        child.next_phase_due_at = None;
    }
}

/// One array job of `size` children on a queue with no capacity of its own, so every child
/// rests where the submit put it; answers the parent's job id and the queue's ARN.
async fn resting_array(batch: &Batch, label: &str, size: usize) -> (String, String) {
    let environment = compute_environment(batch, &format!("ce-{label}")).await;
    let queue_arn = job_queue(batch, &format!("q-{label}"), &environment).await;
    let (definition, _) = job_definition(batch, &format!("jd-{label}"), container_properties()).await;
    let parent_id = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": format!("shards-{label}"),
                "jobQueue": &queue_arn,
                "jobDefinition": &definition,
                "arrayProperties": {"size": size},
            }),
        )
        .await["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label}: expected the array parent's job id"))
        .to_string();
    until_the_clock_owes_nothing(batch, label, &parent_id).await;
    (parent_id, queue_arn)
}

/// RULE: a listing with no `filters` answers the jobs its selector chose that are in ONE
/// status — the one the request names, or `RUNNING` when it names none — whichever selector
/// chose them.
#[tokio::test]
async fn a_listing_narrows_to_one_status_whichever_selector_chose_the_jobs() {
    let batch = Batch::new();
    // One child per modelled status, so every row below asks for a status exactly one job is
    // in and the unfiltered rows have both a RUNNING job to find and six others to hide.
    let (parent_id, queue_arn) = resting_array(&batch, "narrow", JOB_STATUSES.len()).await;
    let at = state::now_millis();
    pin_children(&batch, "one child per status", &parent_id, &JOB_STATUSES, at);

    // The id of the child pinned in `status`.
    let child_of = |status: &str| {
        let index = JOB_STATUSES
            .iter()
            .position(|candidate| *candidate == status)
            .unwrap_or_else(|| panic!("{status} is one of the pinned statuses"));
        state::array_child_job_id(&parent_id, i32::try_from(index).unwrap_or(0))
    };

    // The parent's own status is what its children add up to: live children, so PENDING. The
    // queue listings below see it under that status and under no other, so the rows state it
    // rather than assume it.
    let parent = describe_job(&batch, &parent_id).await;
    assert_eq!(
        parent["status"].as_str(),
        Some(PENDING),
        "case the parent: an array parent with live children is {PENDING}, got {parent}"
    );

    struct Row {
        case: String,
        request: Value,
        /// The job ids the listing must answer, in ascending order.
        expected: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();

    // The selector the issue's caller used, with no status at all: only the RUNNING child.
    rows.push(Row {
        case: "by arrayJobId with no status".to_string(),
        request: json!({"arrayJobId": &parent_id}),
        expected: vec![child_of(RUNNING)],
    });
    // ...and the same listing with each modelled status named: the one child in it.
    for status in JOB_STATUSES {
        rows.push(Row {
            case: format!("by arrayJobId with jobStatus {status}"),
            request: json!({"arrayJobId": &parent_id, "jobStatus": status}),
            expected: vec![child_of(status)],
        });
    }
    // The queue selector narrows identically. Its jobs are the children PLUS the parent, which
    // is a job of the queue too, so the PENDING row answers two and the others one.
    rows.push(Row {
        case: "of the queue with no status".to_string(),
        request: json!({"jobQueue": &queue_arn}),
        expected: vec![child_of(RUNNING)],
    });
    for status in JOB_STATUSES {
        let mut expected = vec![child_of(status)];
        if status == PENDING {
            expected.push(parent_id.clone());
        }
        expected.sort_unstable();
        rows.push(Row {
            case: format!("of the queue with jobStatus {status}"),
            request: json!({"jobQueue": &queue_arn, "jobStatus": status}),
            expected,
        });
    }
    // A selector that chose nothing answers nothing, whether or not a status is named: this
    // parent is an array parent and not a multi-node one.
    rows.push(Row {
        case: "by multiNodeJobId with no status".to_string(),
        request: json!({"multiNodeJobId": &parent_id}),
        expected: Vec::new(),
    });
    rows.push(Row {
        case: format!("by multiNodeJobId with jobStatus {RUNNING}"),
        request: json!({"multiNodeJobId": &parent_id, "jobStatus": RUNNING}),
        expected: Vec::new(),
    });

    for row in &rows {
        let case = &row.case;
        let found = listed(&batch, case, row.request.clone()).await;
        assert_eq!(
            ids(&found),
            row.expected,
            "case {case}: expected the jobs {:?}, got {found:?}",
            row.expected
        );
    }
}

/// RULE (the other half of the same default): the status a listing that names none narrows to
/// is `RUNNING`, so such a listing REPORTS the selected jobs that are running and stops
/// reporting one as soon as it is not.
#[tokio::test]
async fn an_unfiltered_listing_reports_the_selected_jobs_that_are_running() {
    let batch = Batch::new();
    // The smallest array a submit can name, so the count the rule is asserted over differs
    // from the one above. Source: `ArrayProperties$size` — "The array size can be between 2
    // and 10,000" (`state::MIN_ARRAY_SIZE`).
    let size = usize::try_from(state::MIN_ARRAY_SIZE).unwrap_or(2);
    let (parent_id, queue_arn) = resting_array(&batch, "running", size).await;
    let at = state::now_millis();

    // Every child RUNNING: the unfiltered listing answers all of them, under either selector.
    let all_running: Vec<&str> = vec![RUNNING; size];
    pin_children(&batch, "every child running", &parent_id, &all_running, at);
    let mut every_child: Vec<String> = (0..size)
        .map(|index| state::array_child_job_id(&parent_id, i32::try_from(index).unwrap_or(0)))
        .collect();
    every_child.sort_unstable();
    for (case, request) in [
        ("every child running, by arrayJobId", json!({"arrayJobId": &parent_id})),
        ("every child running, of the queue", json!({"jobQueue": &queue_arn})),
    ] {
        let found = listed(&batch, case, request).await;
        assert_eq!(
            ids(&found),
            every_child,
            "case {case}: expected every child, got {found:?}"
        );
    }

    // One of them leaves RUNNING: it leaves the unfiltered listing with it, and is found under
    // the status it is now in.
    let mut mixed = all_running.clone();
    let last = size - 1;
    mixed[last] = "FAILED";
    pin_children(&batch, "one child failed", &parent_id, &mixed, at);
    let still_running: Vec<String> = (0..last)
        .map(|index| state::array_child_job_id(&parent_id, i32::try_from(index).unwrap_or(0)))
        .collect();
    let found = listed(&batch, "one child failed, by arrayJobId", json!({"arrayJobId": &parent_id}))
        .await;
    assert_eq!(
        ids(&found),
        still_running,
        "case one child failed, by arrayJobId: expected only the running children, got {found:?}"
    );
    let found = listed(
        &batch,
        "the failed child by its own status",
        json!({"arrayJobId": &parent_id, "jobStatus": "FAILED"}),
    )
    .await;
    assert_eq!(
        ids(&found),
        vec![state::array_child_job_id(&parent_id, i32::try_from(last).unwrap_or(0))],
        "case the failed child by its own status: expected the one child that failed, got {found:?}"
    );
}
