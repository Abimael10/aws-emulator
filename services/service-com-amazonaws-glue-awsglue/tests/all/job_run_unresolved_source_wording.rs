// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a Glue job run reports a SOURCE PATH its read resolved no file at.
//!
//! The rule under test: a read a job run's script makes RESOLVES its source path before it
//! plans anything over it, and a path it resolved no file at — the store holds no data object
//! under it, or the role the run is performed as is refused the objects there — is reported as
//! the missing-path failure of the READER that resolved it, naming that path, with nothing
//! written where the run's output would have gone. The AWS Glue ETL library's own Amazon S3
//! source lets the Hadoop-compatible filesystem's own failure travel
//! (`java.io.FileNotFoundException: No such file or directory '<path>'`), while Spark's own
//! dataset reader fails its analysis over such a path (`Path does not exist: <path>`) — one
//! condition, worded by each engine as it words it.
//!
//! The rows differ along the two things the rule turns on: WHICH reader resolves the source
//! (the Glue ETL library's S3 source, Spark's dataset reader) and WHY the path resolved no
//! file (the store holds nothing under it, it holds only the markers a write leaves beside its
//! data, or the run's role may not read the objects that are there) — with the permitted read
//! of a source really holding records as the control on each reader, so the rule is not
//! satisfied by a reader that fails every path.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, the script and the
//! data it reads are staged through Amazon S3's own `PutObject`, and each run is read back
//! until it has come to rest ([`crate::job_runs::run_at_rest`]) — so no outcome here depends
//! on how fast the host runs.

use serde_json::Value;
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::job_run_execution_role::{emulator_with_iam_and_s3, grant_role, statement};
use crate::job_run_source_authorization::{
    job_body, output_prefix, part_key, source_prefix, unresolved_signature, COUNTS_ITS_SOURCE,
    COUNTS_ITS_SOURCE_THROUGH_A_FRAME, FRAME_READER, SOURCE_CSV,
};
use crate::job_runs::run_at_rest;
use crate::job_script_execution::{create_bucket, object_body, stage_script, BUCKET, SCRIPTS};
use crate::wire::ok;

/// The label of the rows driven through Spark's own dataset reader.
const DATASET_READER: &str = "dataset";

/// What the store holds under a row's source prefix.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// The CSV object the control reads: three records.
    Records,
    /// Nothing at all: the prefix names no object of the store.
    Nothing,
    /// Only the marker a Spark write leaves beside its data, which holds none of it.
    OnlyAMarker,
}

/// One row: a reader, what its source prefix holds, and whether the run's role may read it.
#[derive(Debug)]
struct Row {
    /// What every assertion of the row reports, and the path segment its objects live under.
    case: &'static str,
    /// Which reader the row's script resolves its source with.
    reader: &'static str,
    /// What the store holds under the row's source prefix.
    source: Source,
    /// Whether the policies in force on the run's role reach the objects under that prefix.
    may_read_source: bool,
    /// Whether the run must succeed and leave the count of the source's rows behind.
    succeeds: bool,
}

/// The rows: each reader over each reason a source path resolves no file, and the permitted
/// read of a source holding records as that reader's control.
fn rows() -> [Row; 7] {
    [
        Row {
            case: "frame-reads-the-records-it-is-allowed",
            reader: FRAME_READER,
            source: Source::Records,
            may_read_source: true,
            succeeds: true,
        },
        Row {
            case: "frame-over-a-prefix-holding-nothing",
            reader: FRAME_READER,
            source: Source::Nothing,
            may_read_source: true,
            succeeds: false,
        },
        Row {
            case: "frame-over-a-prefix-holding-only-a-marker",
            reader: FRAME_READER,
            source: Source::OnlyAMarker,
            may_read_source: true,
            succeeds: false,
        },
        Row {
            case: "frame-over-records-its-role-may-not-read",
            reader: FRAME_READER,
            source: Source::Records,
            may_read_source: false,
            succeeds: false,
        },
        Row {
            case: "dataset-reads-the-records-it-is-allowed",
            reader: DATASET_READER,
            source: Source::Records,
            may_read_source: true,
            succeeds: true,
        },
        Row {
            case: "dataset-over-a-prefix-holding-nothing",
            reader: DATASET_READER,
            source: Source::Nothing,
            may_read_source: true,
            succeeds: false,
        },
        Row {
            case: "dataset-over-records-its-role-may-not-read",
            reader: DATASET_READER,
            source: Source::Records,
            may_read_source: false,
            succeeds: false,
        },
    ]
}

/// The script the row's reader runs.
fn script_of(reader: &str) -> &'static str {
    if reader == FRAME_READER {
        COUNTS_ITS_SOURCE_THROUGH_A_FRAME
    } else {
        COUNTS_ITS_SOURCE
    }
}

/// The statements in force on one row's role: the script read and the output write always, and
/// the source objects only for a row whose role may read them.
fn grants(may_read_source: bool) -> Vec<Value> {
    let mut documents = vec![
        statement("Allow", &["s3:GetObject"], &format!("arn:aws:s3:::{BUCKET}/{SCRIPTS}*")),
        statement("Allow", &["s3:PutObject"], &format!("arn:aws:s3:::{BUCKET}/output/*")),
        // "s3:ListBucket" is what a read of a source DIRECTORY needs beside the object reads:
        // the objects under the prefix are listed before they can be read.
        statement("Allow", &["s3:ListBucket"], &format!("arn:aws:s3:::{BUCKET}")),
    ];
    if may_read_source {
        documents.push(statement(
            "Allow",
            &["s3:GetObject"],
            &format!("arn:aws:s3:::{BUCKET}/data/*"),
        ));
    }
    documents
}

/// Run one row and assert what its run came to.
async fn assert_row(row: &Row, index: usize) {
    let case = row.case;
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    let role_name = format!("job-unresolved-source-role-{index}");
    let role = seed_role(&engine, &role_name, Some(glue_trust()));
    grant_role(&engine, &role_name, &grants(row.may_read_source), &[]);

    match row.source {
        Source::Records => {
            stage_script(&router, &format!("data/{case}/input.csv"), SOURCE_CSV).await;
        }
        Source::Nothing => {}
        Source::OnlyAMarker => {
            // The marker a Spark write leaves beside its data, which holds none of the data:
            // a prefix holding only one resolves no file for a read.
            stage_script(&router, &format!("data/{case}/_SUCCESS"), "done\n").await;
        }
    }

    let name = format!("job-unresolved-{case}");
    let location = stage_script(
        &router,
        &format!("{SCRIPTS}{name}.py"),
        script_of(row.reader),
    )
    .await;
    ok(&router, "CreateJob", &job_body(&name, &role, &location)).await;

    let started = ok(
        &router,
        "StartJobRun",
        &serde_json::json!({
            "JobName": name,
            "Arguments": {
                "--SOURCE_PATH": source_prefix(case),
                "--OUTPUT_PATH": output_prefix(case),
            },
        }),
    )
    .await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
        .to_string();
    let run = run_at_rest(&router, &engine, case, &name, &run_id).await;
    let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
    let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
    let written = object_body(&router, &part_key(case)).await;

    if row.succeeds {
        assert_eq!(
            state, JOB_RUN_STATE_SUCCEEDED,
            "case {case:?}: expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
        );
        assert_eq!(
            written.as_deref(),
            Some("3\n"),
            "case {case:?}: expected the count of the source's rows, got {written:?}"
        );
        return;
    }
    assert_eq!(
        state, JOB_RUN_STATE_FAILED,
        "case {case:?}: expected {JOB_RUN_STATE_FAILED}, got {run}"
    );
    // The words are the reader's own for a path it resolved no file at, whichever reason the
    // path resolved none.
    let signature = unresolved_signature(row.reader);
    assert!(
        message.contains(signature),
        "case {case:?}: expected an ErrorMessage carrying {signature:?}, got {message:?}"
    );
    // And the path the read was GIVEN is the one it names, which is the one variable part of
    // that failure.
    let named = source_prefix(case);
    assert!(
        message.contains(&named),
        "case {case:?}: expected the ErrorMessage to name {named:?}, got {message:?}"
    );
    assert!(
        written.is_none(),
        "case {case:?}: a run whose source resolved no file wrote no output, got {written:?}"
    );
}

/// A source path a read resolved no file at is reported in the words of the reader that
/// resolved it, naming that path, whether the store held nothing under it or the run's role was
/// refused what was there — and a reader given a source that really holds records still reads
/// them.
///
/// One test, so the rows run one at a time: each one executes a script, and the interpreters
/// this emulator runs scripts on are shared by every suite.
#[tokio::test]
async fn a_source_that_resolved_no_file_is_reported_in_its_readers_own_words() {
    for (index, row) in rows().iter().enumerate() {
        assert_row(row, index).await;
    }
}
