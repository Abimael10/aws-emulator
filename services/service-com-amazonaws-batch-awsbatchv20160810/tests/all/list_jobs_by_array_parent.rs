// SPDX-License-Identifier: CC0-1.0

//! Listing the children of an ARRAY PARENT, and what a `SEQUENTIAL` dependency orders — end to
//! end over the wire.
//!
//! Two rules are under test, each over a set of inputs.
//!
//! **WHICH selector a `ListJobs` names decides whether a status is implied.**
//! `ListJobsRequest$arrayJobId` is "the job ID for an array job. Specifying an array job ID
//! with this parameter lists ALL CHILD JOBS from within the specified array", unconditionally,
//! and `$multiNodeJobId` "lists all nodes that are associated with the specified job"; the
//! implicit status belongs to the other selector — `$jobStatus` is "the job status used to
//! filter jobs in the specified QUEUE … If you don't specify a status, only `RUNNING` jobs are
//! returned". So a listing by parent id answers every child whatever status it is in, a listing
//! of a queue that names no status answers its `RUNNING` jobs, and a status the caller DID name
//! selects in either listing. That is what lets a client enumerate the children of an array job
//! none of whose children is running.
//!
//! **A `SEQUENTIAL` dependency orders the array's own children.** `SubmitJob$dependsOn`: "you
//! can specify a `SEQUENTIAL` type dependency without specifying a job ID for array jobs so
//! that each child array job completes sequentially, starting at index 0. You can also specify
//! an `N_TO_N` type dependency WITH A JOB ID for array jobs. In that case, each index child of
//! this job must wait for the corresponding index child of each dependency". The TYPE is what
//! says which form an entry is, so a `SEQUENTIAL` entry orders the children by index whether or
//! not it also carries a job id — and a job id it does carry is still a job the whole array
//! waits for. A dependency naming the array's OWN PARENT is inherited by nothing: a parent
//! reaches a terminal status only once every child has, so a child held on it could never run.
//!
//! The rows differ along what the rules generalise over: which selector the listing names and
//! whether it names a status; and, for the dependencies, the TYPE of the entry, whether it
//! carries a job id, which job that id names (another job, the array's own parent) and the
//! child's own index.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    compute_environment, container_properties, job_definition, job_queue, until_status, Batch,
};

/// How many children the arrays below have.
const SIZE: i64 = 3;

/// The `JobStatus` members this suite selects by, spelled out rather than imported so a wrong
/// constant in the implementation cannot make it pass. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const RUNNING: &str = "RUNNING";
const PENDING: &str = "PENDING";

/// The `ArrayJobDependency` members. Source:
/// `smithy_query(query="shape",shape="ArrayJobDependency")`.
const SEQUENTIAL: &str = "SEQUENTIAL";
const N_TO_N: &str = "N_TO_N";

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

/// The `arrayProperties.index` of each listed job, in ascending order.
fn indexes(listed: &[Value]) -> Vec<i64> {
    let mut found: Vec<i64> = listed
        .iter()
        .filter_map(|job| job["arrayProperties"]["index"].as_i64())
        .collect();
    found.sort_unstable();
    found
}

/// RULE: a listing by `arrayJobId` answers every child of that parent whatever status it is in,
/// while a listing of a QUEUE that names no status answers only its `RUNNING` jobs; a status the
/// caller names selects in either listing.
#[tokio::test]
async fn a_listing_by_array_parent_answers_every_child_whatever_its_status() {
    let batch = Batch::new();
    let environment = managed_compute_environment(&batch, "ce-list-array", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-list-array", &environment).await;
    let (definition, _) = job_definition(&batch, "jd-list-array", container_properties()).await;
    let answered = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": "shards",
                "jobQueue": &queue_arn,
                "jobDefinition": &definition,
                "arrayProperties": {"size": SIZE},
            }),
        )
        .await;
    let parent_id = answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case the array: expected a jobId, got {answered}"))
        .to_string();

    // Every child runs its container and ends, so by the time the listings below are taken
    // none of them is RUNNING — which is exactly the state a RUNNING-only listing hides.
    for index in 0..SIZE {
        let child_id = state::array_child_job_id(&parent_id, i32::try_from(index).unwrap_or(0));
        until_status(&batch, &format!("child {index}"), &child_id, SUCCEEDED).await;
    }
    until_status(&batch, "the parent", &parent_id, SUCCEEDED).await;

    struct Row {
        case: &'static str,
        /// The `ListJobs` request, minus the ids this test mints.
        request: fn(&str, &str) -> Value,
        /// The child indexes the listing must answer, in ascending order.
        indexes: &'static [i64],
        /// How many jobs it must answer in all — the children plus, where the listing selects
        /// the queue, the array PARENT, which is a job of that queue too.
        total: usize,
    }

    let rows: &[Row] = &[
        Row {
            case: "by arrayJobId with no status at all",
            request: |parent, _| json!({"arrayJobId": parent}),
            indexes: &[0, 1, 2],
            total: 3,
        },
        Row {
            case: "by arrayJobId naming the status every child is in",
            request: |parent, _| json!({"arrayJobId": parent, "jobStatus": SUCCEEDED}),
            indexes: &[0, 1, 2],
            total: 3,
        },
        Row {
            case: "by arrayJobId naming a status no child is in",
            request: |parent, _| json!({"arrayJobId": parent, "jobStatus": RUNNING}),
            indexes: &[],
            total: 0,
        },
        Row {
            case: "by arrayJobId naming another status no child is in",
            request: |parent, _| json!({"arrayJobId": parent, "jobStatus": PENDING}),
            indexes: &[],
            total: 0,
        },
        Row {
            case: "of the queue with no status, which answers its RUNNING jobs",
            request: |_, queue| json!({"jobQueue": queue}),
            indexes: &[],
            total: 0,
        },
        Row {
            case: "of the queue naming the status its jobs are in",
            request: |_, queue| json!({"jobQueue": queue, "jobStatus": SUCCEEDED}),
            indexes: &[0, 1, 2],
            total: 4,
        },
        Row {
            case: "by multiNodeJobId, which this parent is not",
            request: |parent, _| json!({"multiNodeJobId": parent}),
            indexes: &[],
            total: 0,
        },
    ];

    for row in rows {
        let case = row.case;
        let found = listed(&batch, case, (row.request)(&parent_id, &queue_arn)).await;
        assert_eq!(
            found.len(),
            row.total,
            "case {case}: expected {} job(s), got {found:?}",
            row.total
        );
        assert_eq!(
            indexes(&found),
            row.indexes.to_vec(),
            "case {case}: expected the children {:?}, got {found:?}",
            row.indexes
        );
    }
}

/// RULE: a `SEQUENTIAL` entry makes each child wait for the index before it, whatever job id the
/// entry carries, and an entry naming a job outside the array is inherited beside that
/// ordering; an `N_TO_N` entry is re-pointed at the depended-on array's child of the same index.
#[tokio::test]
async fn a_sequential_dependency_orders_the_children_whatever_job_id_it_carries() {
    // A queue with no capacity of its own, so every job below rests where it was put and the
    // dependencies a submit recorded are read back unchanged.
    let batch = Batch::new();
    let environment = compute_environment(&batch, "ce-deps").await;
    let queue_arn = job_queue(&batch, "q-deps", &environment).await;
    let (definition, _) = job_definition(&batch, "jd-deps", container_properties()).await;

    // One plain job for the entries that name a job OUTSIDE the array, and one array parent for
    // the `N_TO_N` pairing to point into.
    let other = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": "other",
                "jobQueue": &queue_arn,
                "jobDefinition": &definition,
            }),
        )
        .await["jobId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let other_array = batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": "other-array",
                "jobQueue": &queue_arn,
                "jobDefinition": &definition,
                "arrayProperties": {"size": SIZE},
            }),
        )
        .await["jobId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    struct Row {
        case: &'static str,
        /// The `dependsOn` the array submit carries, built from the ids above.
        depends_on: fn(&str, &str) -> Value,
        /// What each child's own `dependsOn` must then be, by index: the ids are built the same
        /// way, with `parent` the array's own parent id.
        expected: fn(&str, &str, &str, i64) -> Vec<Value>,
    }

    let rows: &[Row] = &[
        Row {
            case: "a SEQUENTIAL entry with no job id",
            depends_on: |_, _| json!([{"type": SEQUENTIAL}]),
            expected: |parent, _, _, index| sequential_chain(parent, index),
        },
        Row {
            case: "a SEQUENTIAL entry that also names another job",
            depends_on: |other, _| json!([{"jobId": other, "type": SEQUENTIAL}]),
            expected: |parent, other, _, index| {
                let mut wanted = sequential_chain(parent, index);
                wanted.push(json!({"jobId": other, "type": SEQUENTIAL}));
                wanted
            },
        },
        Row {
            case: "an N_TO_N entry naming an array of the same size",
            depends_on: |_, other_array| json!([{"jobId": other_array, "type": N_TO_N}]),
            expected: |_, _, other_array, index| {
                vec![json!({"jobId": format!("{other_array}:{index}"), "type": N_TO_N})]
            },
        },
        Row {
            case: "an entry naming a job outside the array, with no type at all",
            depends_on: |other, _| json!([{"jobId": other}]),
            expected: |_, other, _, _| vec![json!({"jobId": other})],
        },
    ];

    for (row_index, row) in rows.iter().enumerate() {
        let case = row.case;
        let parent_id = batch
            .ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("deps-{row_index}"),
                    "jobQueue": &queue_arn,
                    "jobDefinition": &definition,
                    "arrayProperties": {"size": SIZE},
                    "dependsOn": (row.depends_on)(&other, &other_array),
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId"))
            .to_string();
        for index in 0..SIZE {
            let child_id = state::array_child_job_id(&parent_id, i32::try_from(index).unwrap_or(0));
            let described = batch
                .ok("/v1/describejobs", json!({"jobs": [&child_id]}))
                .await;
            let child = described["jobs"][0].clone();
            let wanted = (row.expected)(&parent_id, &other, &other_array, index);
            assert_eq!(
                child["dependsOn"],
                Value::Array(wanted.clone()),
                "case {case}: expected child {index} to depend on {wanted:?}, got {child}"
            );
        }
    }
}

/// What a `SEQUENTIAL` entry orders: the child at `index` waits for the one before it, and the
/// child at index 0 waits for nothing — "each child array job completes sequentially, starting
/// at index 0".
fn sequential_chain(parent: &str, index: i64) -> Vec<Value> {
    if index == 0 {
        return Vec::new();
    }
    vec![json!({"jobId": format!("{parent}:{}", index - 1), "type": SEQUENTIAL})]
}
