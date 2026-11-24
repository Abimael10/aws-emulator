// SPDX-License-Identifier: CC0-1.0

//! What each CHILD of an array job RUNS, and what it reports from that run, end to end over
//! the wire.
//!
//! RULE under test: every child of an array job is an independent container execution of the
//! one program its job definition declares, performed with the reserved array-index variable
//! set to that child's own zero-based `ArrayPropertiesDetail$index` ("the job index within
//! the array that's associated with this job ... returned for array job children") — so each
//! child reports its own index, the exit status that program ended with
//! (`ContainerDetail$exitCode`, "the exit code returned upon completion") and hence its own
//! terminal `JobStatus`, writes the lines it printed into a log stream of its own
//! (`ContainerDetail$logStreamName`, "the name of the Amazon CloudWatch Logs log stream
//! that's associated with the container ... Each container attempt receives a log stream name
//! when they reach the RUNNING status"), and the parent's
//! `ArrayPropertiesDetail$statusSummary` ("a summary of the number of array job children in
//! each available job status ... returned for parent array jobs") counts those real outcomes.
//!
//! The rows differ along what the rule generalises over: the array's SIZE, and how the
//! program's exit status depends on the index it reads — the same for every child, one per
//! child, or one for half of them. The `environment` doc reserves the `AWS_BATCH` prefix for
//! "variables that Batch sets", which is what makes a program keyed on the index observable
//! at all: the value it reads is the only thing that tells one child's work from another's.
//!
//! Sources: `smithy_query(query="shape",shape="ArrayProperties,ArrayPropertiesDetail")`,
//! `smithy_query(query="shape",shape="ContainerDetail")` for `exitCode`/`logStreamName`, and
//! `smithy_query(query="shape",shape="JobStatus")` for the statuses.
//!
//! Where a placed job's stream LIVES (the group each log driver delivers to) is another
//! rule, pinned in `job_log_streams.rs`; what this suite adds is what the stream HOLDS and
//! which child's index decided it.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{job_definition, job_queue, until_status, Batch};

/// The statuses and summary keys this suite waits for, spelled out rather than imported so a
/// wrong constant in the implementation cannot make it pass. Source:
/// `smithy_query(query="shape",shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The `JobStatus` values no child of a settled array is in any more.
const LIVE_STATUSES: &[&str] = &["SUBMITTED", "PENDING", "RUNNABLE", "STARTING", "RUNNING"];

/// The log group the children's `awslogs` configuration delivers to, created through
/// CloudWatch Logs' own API before any array is submitted, and the stream prefix it names.
const LOG_GROUP: &str = "/emulator/batch/array-index";
const STREAM_PREFIX: &str = "shard";

/// How many times a poll looks before it gives up. Each clock-owed move falls due
/// `state::JOB_PHASE_MILLIS` after the one before it and a child makes a handful of them, so
/// this is many spans of headroom on a loaded machine.
const POLLS: usize = 480;

/// How long one look waits before the next, well under one clock-owed span.
fn poll_interval() -> Duration {
    let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
    Duration::from_millis((span / 10).max(5))
}

/// What one row of the table submits, and what each of its children must then report.
struct Row {
    case: &'static str,
    /// `ArrayProperties$size` — how many children the submit spawns.
    size: i32,
    /// The program every child runs, as the `sh -c` text of its container's command.
    program: &'static str,
    /// The status that program exits with for the child at `index`.
    exit_code: fn(i32) -> i32,
}

/// The reserved variable a child's container reads its own index from. Source: the AWS Batch
/// User Guide's reserved variables, under `ContainerProperties$environment`'s "This naming
/// convention is reserved for variables that Batch sets".
const INDEX_VARIABLE: &str = "AWS_BATCH_JOB_ARRAY_INDEX";

const ROWS: &[Row] = &[
    Row {
        case: "two children whose program exits zero whatever its index",
        size: 2,
        program: "echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX",
        exit_code: |_| 0,
    },
    Row {
        case: "three children whose exit status IS the index each one read",
        size: 3,
        program: "echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX; exit $((AWS_BATCH_JOB_ARRAY_INDEX))",
        exit_code: |index| index,
    },
    Row {
        case: "four children of which the odd-indexed half fail",
        size: 4,
        program: "echo IDX=$AWS_BATCH_JOB_ARRAY_INDEX; exit $((AWS_BATCH_JOB_ARRAY_INDEX % 2))",
        exit_code: |index| index % 2,
    },
];

/// `containerProperties` running `program` under `sh -c`, delivering its output to
/// [`LOG_GROUP`] through the `awslogs` driver.
fn container_running(program: &str) -> Value {
    json!({
        "image": "busybox",
        "command": ["sh", "-c", program],
        "resourceRequirements": [
            {"type": "VCPU", "value": "1"},
            {"type": "MEMORY", "value": "512"},
        ],
        "logConfiguration": {
            "logDriver": "awslogs",
            "options": {
                "awslogs-group": LOG_GROUP,
                "awslogs-stream-prefix": STREAM_PREFIX,
            },
        },
    })
}

/// The messages CloudWatch Logs holds in `stream`, oldest first — and none at all for a
/// stream it does not hold yet.
async fn delivered(batch: &Batch, stream: &str) -> Vec<String> {
    let (status, body) = batch
        .logs(
            "GetLogEvents",
            json!({
                "logGroupName": LOG_GROUP,
                "logStreamName": stream,
                "startFromHead": true,
            }),
        )
        .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Poll until `stream` holds exactly one line, and answer it.
async fn until_one_line(batch: &Batch, case: &str, stream: &str) -> String {
    for _ in 0..POLLS {
        let held = delivered(batch, stream).await;
        if held.len() == 1 {
            return held[0].clone();
        }
        tokio::time::sleep(poll_interval()).await;
    }
    panic!(
        "case {case}: expected {stream} to hold the one line its container printed, got {:?}",
        delivered(batch, stream).await
    );
}

/// The integer a `IDX=<n>` line carries, which is what the child's container printed from the
/// index it read.
fn printed_index(case: &str, line: &str) -> i64 {
    let digits = line.strip_prefix("IDX=").unwrap_or_else(|| {
        panic!("case {case}: expected a line of the IDX=<n> form, got {line:?}")
    });
    assert!(
        !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit()),
        "case {case}: expected {INDEX_VARIABLE} to be printed as a digit string, got {line:?}"
    );
    digits
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("case {case}: expected a number in {line:?}"))
}

/// RULE: each array child runs the one declared program with its OWN index, so its index, its
/// exit code, its terminal status, its own log stream and the line that stream holds all
/// follow from that index — and the parent's `statusSummary` counts what the children really
/// did.
#[tokio::test]
async fn each_array_child_runs_the_declared_program_with_its_own_index() {
    let batch = Batch::new();
    let (status, body) = batch
        .logs("CreateLogGroup", json!({"logGroupName": LOG_GROUP}))
        .await;
    assert_eq!(
        status, 200,
        "setup: CreateLogGroup {LOG_GROUP:?} answered {status} {body}"
    );
    let environment =
        managed_compute_environment(&batch, "ce-array-index", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-array-index", &environment).await;

    for (row_index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (definition, _) = job_definition(
            &batch,
            &format!("jd-array-index-{row_index}"),
            container_running(row.program),
        )
        .await;
        let answered = batch
            .ok(
                "/v1/submitjob",
                json!({
                    "jobName": format!("shards-{row_index}"),
                    "jobQueue": &queue_arn,
                    "jobDefinition": definition,
                    "arrayProperties": {"size": row.size},
                }),
            )
            .await;
        let parent_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case}: expected the array parent's jobId, got {answered}")
            })
            .to_string();

        let mut streams: Vec<String> = Vec::new();
        let mut succeeded = 0;
        for index in 0..row.size {
            let child_case = format!("{case}: child {index}");
            let child_id = state::array_child_job_id(&parent_id, index);
            let expected_code = (row.exit_code)(index);
            let expected_status = if expected_code == 0 { SUCCEEDED } else { FAILED };
            succeeded += i64::from(expected_code == 0);

            let detail = until_status(&batch, &child_case, &child_id, expected_status).await;
            assert_eq!(
                detail["arrayProperties"]["index"].as_i64(),
                Some(i64::from(index)),
                "case {child_case}: expected the child to report its own index, got {detail}"
            );
            assert_eq!(
                detail["container"]["exitCode"].as_i64(),
                Some(i64::from(expected_code)),
                "case {child_case}: expected the exit code its program ended with, got {detail}"
            );
            let stream = detail["container"]["logStreamName"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {child_case}: expected the container to name its log stream, got \
                         {detail}"
                    )
                })
                .to_string();
            assert_eq!(
                detail["attempts"][0]["container"]["logStreamName"].as_str(),
                Some(stream.as_str()),
                "case {child_case}: the attempt and the container report one stream, got {detail}"
            );

            // What the container PRINTED is the index the API reports for this same child.
            let line = until_one_line(&batch, &child_case, &stream).await;
            assert_eq!(
                printed_index(&child_case, &line),
                i64::from(index),
                "case {child_case}: expected the printed {INDEX_VARIABLE} to equal the child's \
                 own index, got {line:?}"
            );
            streams.push(stream);
        }

        let mut unique = streams.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            streams.len(),
            "case {case}: each child writes to a stream of its own, got {streams:?}"
        );

        // The parent is what its children add up to: terminal SUCCEEDED only when every one
        // of them exited zero, with the summary counting the real outcomes.
        let parent_status = if succeeded == i64::from(row.size) {
            SUCCEEDED
        } else {
            FAILED
        };
        let parent_case = format!("{case}: the parent");
        let parent = until_status(&batch, &parent_case, &parent_id, parent_status).await;
        let summary = &parent["arrayProperties"]["statusSummary"];
        assert_eq!(
            parent["arrayProperties"]["size"].as_i64(),
            Some(i64::from(row.size)),
            "case {case}: expected the parent to report the array's size, got {parent}"
        );
        assert_eq!(
            summary[SUCCEEDED].as_i64(),
            Some(succeeded),
            "case {case}: expected {succeeded} children counted as {SUCCEEDED}, got {parent}"
        );
        assert_eq!(
            summary[FAILED].as_i64(),
            Some(i64::from(row.size) - succeeded),
            "case {case}: expected the rest counted as {FAILED}, got {parent}"
        );
        for live in LIVE_STATUSES {
            assert_eq!(
                summary[*live].as_i64(),
                Some(0),
                "case {case}: every child is terminal, so {live} counts none, got {parent}"
            );
        }
    }
}
