// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TERMINAL state a Glue ETL job run settles in when the role the run
//! is performed as is refused the read of the run's SOURCE, however that refusal is written.
//!
//! The rule under test: every read a job run's script makes is signed as the job's `Role` ("the
//! name or Amazon Resource Name (ARN) of the IAM role associated with this job", `Job$Role`),
//! so a source read the policies in force on that role do not permit is refused whichever shape
//! the refusal takes, and the reader that resolved the path reports it as a path it found no
//! file at — the run therefore comes to REST in the modelled failure state
//! (`JobRun$JobRunState` `FAILED`), carries that reader's own missing-path failure naming the
//! source in `JobRun$ErrorMessage`, and leaves NOTHING under the prefix its output would have
//! gone to. The same submission under a role the policies permit the read to settles
//! `SUCCEEDED` and leaves, under that prefix, the value its own read of the source computed.
//!
//! Both halves matter to a client: a run that settles in a terminal state is one a poll of
//! `GetJobRun` makes progress on, and a run whose role was misconfigured is one that reads
//! differently from a correctly permissioned one rather than identically to it.
//!
//! The rows differ along what the rule generalises over: HOW the refusal of the source read is
//! written — an explicit deny over the source PREFIX (beside the narrow allows a job role is
//! routinely given for its script and its output), an explicit deny naming the source OBJECT
//! exactly, and an explicit deny written with a wildcard INSIDE the key — and, for the
//! permitted arm, how many records the source really holds, so the value the run writes is one
//! only a real read of that source produces. The recorded case — a Glue 4.0 `glueetl` job on
//! `G.1X` workers with `MaxRetries` 0, whose role allows the script read and the output write
//! and explicitly denies `s3:GetObject` over the source prefix — is one row of the table.
//!
//! What [`crate::job_run_source_authorization`] already spans is NOT repeated here: the
//! refusal written as IAM's implicit deny (no statement allowing the read) and the one written
//! by leaving the bucket listing out are its rows, as is the shape of a grant over another
//! bucket or through two documents ([`crate::job_run_execution_role`]). What this suite adds is
//! the spellings a deny itself is written in, the second source size, and the state of the
//! whole OUTPUT PREFIX rather than of the one part object under it.
//!
//! The reader is the AWS Glue ETL library's own Amazon S3 source — the one a `DynamicFrame` CSV
//! read sits on, which is the reader the recorded scenario uses — for every row. The other
//! dimension, the READER that resolves the path (Spark's own dataset reader words the same
//! condition in its own engine's words), is the one
//! [`crate::job_run_source_authorization::unresolved_signature`] spans, so it is not repeated
//! here either.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, the script and the
//! data it reads are staged through Amazon S3's own `PutObject`, the IAM records are seeded
//! into the SAME shared engine the service reads them from, and each run is READ BACK until it
//! has come to rest ([`crate::job_runs::run_at_rest`]) — so nothing here sleeps for a fixed
//! time or depends on how fast the host runs.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::job_run_execution_role::{emulator_with_iam_and_s3, grant_role, statement};
use crate::job_run_source_authorization::{
    job_body, output_prefix, part_key, source_prefix, COUNTS_ITS_SOURCE_THROUGH_A_FRAME,
    NO_SUCH_FILE_OR_DIRECTORY,
};
use crate::job_runs::run_at_rest;
use crate::job_script_execution::{create_bucket, object_body, stage_script, BUCKET, SCRIPTS};
use crate::job_script_parquet_output::keys_under;
use crate::wire::ok;

/// The columns this suite's source objects carry, so what a row varies is the number of
/// RECORDS under the source prefix and nothing else.
const SOURCE_HEADER: &str = "id,name,value";

/// The source object one case's prefix holds.
fn source_key(case: &str) -> String {
    format!("data/{case}/input.csv")
}

/// A CSV source holding `records` records under [`SOURCE_HEADER`] — the figure a run that
/// really read it writes out.
fn source_csv(records: usize) -> String {
    let mut text = String::from(SOURCE_HEADER);
    for record in 1..=records {
        text.push_str(&format!("\n{record},name-{record},{}", record * 10));
    }
    text.push('\n');
    text
}

/// One row: how the row's role decides the source read, how many records the source holds, and
/// whether the run must therefore settle in the permitted or the refused arm.
struct Row {
    /// What every assertion of the row reports, and the path segment its objects live under.
    case: &'static str,
    /// How many records the row's source object holds.
    records: usize,
    /// The statements in force on the row's role, as (effect, actions, resource) — the resource
    /// spelling `BUCKET` for this suite's bucket and `CASE` for the row's own case.
    grants: Vec<(&'static str, &'static [&'static str], &'static str)>,
    /// Whether the policies refuse the source read, so the run must settle in the failure arm.
    source_refused: bool,
}

/// The rows: the two arms of the boundary, and every shape the refusal of the source read is
/// written in.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The positive control: a role holding the object reads, the object writes and the
            // bucket listing a source read starts with.
            case: "permitted-over-three-records",
            records: 3,
            grants: vec![
                ("Allow", &["s3:GetObject", "s3:PutObject"], "arn:aws:s3:::BUCKET/*"),
                ("Allow", &["s3:ListBucket"], "arn:aws:s3:::BUCKET"),
            ],
            source_refused: false,
        },
        Row {
            // The same control over a source of another size: what the run writes is the figure
            // its own read computed, not a figure of this suite's.
            case: "permitted-over-eight-records",
            records: 8,
            grants: vec![
                ("Allow", &["s3:GetObject", "s3:PutObject"], "arn:aws:s3:::BUCKET/*"),
                ("Allow", &["s3:ListBucket"], "arn:aws:s3:::BUCKET"),
            ],
            source_refused: false,
        },
        Row {
            // The recorded shape: the script read and the output write allowed narrowly, the
            // bucket listing allowed, and the SOURCE prefix explicitly denied.
            case: "denied-over-the-source-prefix",
            records: 3,
            grants: vec![
                ("Allow", &["s3:ListBucket"], "arn:aws:s3:::BUCKET"),
                (
                    "Allow",
                    &["s3:GetObject", "s3:PutObject"],
                    "arn:aws:s3:::BUCKET/scripts/*",
                ),
                (
                    "Allow",
                    &["s3:GetObject", "s3:PutObject"],
                    "arn:aws:s3:::BUCKET/output/*",
                ),
                ("Deny", &["s3:GetObject"], "arn:aws:s3:::BUCKET/data/*"),
            ],
            source_refused: true,
        },
        Row {
            // The deny names the source OBJECT exactly, beside an allow of every object.
            case: "denied-on-the-source-object-itself",
            records: 3,
            grants: vec![
                ("Allow", &["s3:GetObject", "s3:PutObject"], "arn:aws:s3:::BUCKET/*"),
                ("Allow", &["s3:ListBucket"], "arn:aws:s3:::BUCKET"),
                (
                    "Deny",
                    &["s3:GetObject"],
                    "arn:aws:s3:::BUCKET/data/CASE/input.csv",
                ),
            ],
            source_refused: true,
        },
        Row {
            // The deny is written with a wildcard INSIDE the key, which IAM's `*` spans.
            case: "denied-through-a-wildcard-inside-the-key",
            records: 3,
            grants: vec![
                ("Allow", &["s3:GetObject", "s3:PutObject"], "arn:aws:s3:::BUCKET/*"),
                ("Allow", &["s3:ListBucket"], "arn:aws:s3:::BUCKET"),
                (
                    "Deny",
                    &["s3:GetObject"],
                    "arn:aws:s3:::BUCKET/data/*/input.csv",
                ),
            ],
            source_refused: true,
        },
    ]
}

/// Run one row's job through the AWS Glue ETL library's own source reader, and assert the
/// terminal state, the reported reason and what is under the run's output prefix.
async fn assert_row(row: &Row, index: usize) {
    let case = row.case;
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    let role_name = format!("denied-source-role-{index}");
    let role = seed_role(&engine, &role_name, Some(glue_trust()));
    let documents: Vec<Value> = row
        .grants
        .iter()
        .map(|&(effect, actions, resource)| {
            statement(
                effect,
                actions,
                &resource.replace("BUCKET", BUCKET).replace("CASE", case),
            )
        })
        .collect();
    grant_role(&engine, &role_name, &documents, &[]);

    // The source the run reads, staged through the object store's own `PutObject`, and the
    // script that reads it.
    stage_script(&router, &source_key(case), &source_csv(row.records)).await;
    let name = format!("job-denied-source-{case}");
    let location = stage_script(
        &router,
        &format!("{SCRIPTS}{name}.py"),
        COUNTS_ITS_SOURCE_THROUGH_A_FRAME,
    )
    .await;
    let mut body = job_body(&name, &role, &location);
    // "The maximum number of times to retry this job if it fails" (`Job$MaxRetries`): the
    // recorded submission asks for no retry, so one start is one attempt.
    body["MaxRetries"] = json!(0);
    ok(&router, "CreateJob", &body).await;

    let started = ok(
        &router,
        "StartJobRun",
        &json!({
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

    // The run is polled for the CONDITION a client polls for — that it has reached a state it
    // never leaves again — against the shared generous deadline.
    let run = run_at_rest(&router, &engine, case, &name, &run_id).await;
    let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
    let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
    let written = keys_under(&router, case, &format!("output/{case}/")).await;

    if !row.source_refused {
        assert_eq!(
            state, JOB_RUN_STATE_SUCCEEDED,
            "case {case:?}: expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
        );
        assert!(
            run.get("ErrorMessage").is_none(),
            "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
        );
        // What the run wrote is the number of records its own read of the source found, so the
        // control is backed by a real read rather than by the run's own report of itself.
        let part = object_body(&router, &part_key(case)).await;
        let expected = format!("{}\n", row.records);
        assert_eq!(
            part.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the count of the source's records {expected:?}, got \
             {part:?}"
        );
        assert!(
            written.iter().any(|key| key.contains("/part-")),
            "case {case:?}: expected a part object under the output prefix, got {written:?}"
        );
        return;
    }

    assert_eq!(
        state, JOB_RUN_STATE_FAILED,
        "case {case:?}: expected {JOB_RUN_STATE_FAILED}, got {run}"
    );
    // The reason is the missing-path failure of the reader that resolved the source, naming the
    // path it resolved — the one variable part of it.
    assert!(
        message.contains(NO_SUCH_FILE_OR_DIRECTORY),
        "case {case:?}: expected an ErrorMessage carrying {NO_SUCH_FILE_OR_DIRECTORY:?}, got \
         {message:?}"
    );
    let named = source_prefix(case);
    assert!(
        message.contains(&named),
        "case {case:?}: expected the ErrorMessage to name {named:?}, got {message:?}"
    );
    // A run that could not read its source produced no output at all, so the prefix its output
    // would have gone to holds nothing.
    assert!(
        written.is_empty(),
        "case {case:?}: a run refused its source wrote nothing under its output prefix, got \
         {written:?}"
    );
}

/// A source read the job role's policies refuse settles the run in `FAILED` carrying the
/// reader's missing-path failure for that source, with nothing under the run's output prefix,
/// however the refusal is written — and the same submission under a role that permits the read
/// settles `SUCCEEDED` carrying the figure its own read of the source computed.
///
/// One test, so the rows run one at a time: each row executes a script, and the interpreters
/// this emulator runs scripts on are shared by every suite.
#[tokio::test]
async fn a_run_whose_role_is_refused_its_source_settles_failed_and_writes_nothing() {
    for (index, row) in rows().iter().enumerate() {
        assert_row(row, index).await;
    }
}
