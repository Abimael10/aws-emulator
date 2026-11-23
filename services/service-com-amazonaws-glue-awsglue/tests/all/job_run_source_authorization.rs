// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Glue job run's script is allowed to READ, and for how a
//! source it may not read is reported.
//!
//! The rule under test: every read a job run's script makes is signed as the job's `Role`, so
//! the SOURCE path a read resolves is resolved under that role's effective policies — a path
//! no statement allows is refused exactly as one an explicit deny names is — and a source
//! path the read resolved no file at, whether the store holds nothing under it or the role is
//! refused it, is the missing-path failure its own reader words, naming that path, with
//! nothing written where the run's output would have gone. The two readers a Glue job script
//! has word that one condition as their own engines do: Spark's dataset reader fails its
//! analysis over such a path, while the AWS Glue ETL library's Amazon S3 source lets the
//! filesystem's own missing-path failure travel ([`unresolved_signature`]).
//!
//! The rows differ along HOW the role's documents decide the source read: allowed outright,
//! left out of every statement (an implicit deny, which is the shape of a role that was given
//! the script read and the output write and never the source read), named by an explicit deny
//! beside an allow of everything, and with the LISTING a source read starts with left out.
//! The output write is one further row, because a refused WRITE is reported differently: it
//! names the object it was making, so the store's own refusal stands. The other shapes a
//! grant can take — a wildcard, another bucket's objects, two documents — are spanned by the
//! sibling role table in [`crate::job_run_execution_role`], because the policies are
//! evaluated by the object store either way.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, the script and
//! the data it reads are staged through Amazon S3's own `PutObject`, and each run is read back
//! until it has come to rest ([`crate::job_runs::run_at_rest`]) — so no outcome here depends
//! on how fast the host runs.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::job_run_execution_role::{emulator_with_iam_and_s3, grant_role, statement};
use crate::job_runs::run_at_rest;
use crate::job_script_execution::{
    create_bucket, object_body, stage_script, BUCKET, SCRIPTS,
};
use crate::wire::ok;

/// The fixed text Spark's analysis states for a source path it resolved no file at, written
/// out here rather than read from a crate constant so this suite pins the wording.
pub(crate) const PATH_DOES_NOT_EXIST: &str = "Path does not exist";

/// The fixed text the Hadoop-compatible filesystem states for a path it resolved no file at,
/// which is what the AWS Glue ETL library's own Amazon S3 source reports.
pub(crate) const NO_SUCH_FILE_OR_DIRECTORY: &str = "No such file or directory";

/// The label of the rows driven through the AWS Glue ETL library's own source reader.
pub(crate) const FRAME_READER: &str = "frame";

/// The fixed text the reader behind `label` states for a source path it resolved no file at.
pub(crate) fn unresolved_signature(label: &str) -> &'static str {
    if label == FRAME_READER {
        NO_SUCH_FILE_OR_DIRECTORY
    } else {
        PATH_DOES_NOT_EXIST
    }
}

/// The fixed words the object store's own refusal of a write carries.
const ACCESS_DENIED: &str = "Access Denied";

/// The script every row runs: it reads the CSV source its `--SOURCE_PATH` names and writes
/// the number of rows that source really held under its `--OUTPUT_PATH`.
pub(crate) const COUNTS_ITS_SOURCE: &str = r#"
import sys

from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext
from pyspark.sql import SparkSession

args = getResolvedOptions(sys.argv, ["SOURCE_PATH", "OUTPUT_PATH"])
spark = SparkSession(SparkContext.getOrCreate())
source = spark.read.option("header", "true").csv(args["SOURCE_PATH"])
spark.sparkContext.parallelize([str(source.count())]).saveAsTextFile(args["OUTPUT_PATH"])
"#;

/// The same read through the AWS Glue ETL library's own source reader, so the rule is asserted
/// on both readers a Glue job script has.
pub(crate) const COUNTS_ITS_SOURCE_THROUGH_A_FRAME: &str = r#"
import sys

from awsglue.context import GlueContext
from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext

args = getResolvedOptions(sys.argv, ["SOURCE_PATH", "OUTPUT_PATH"])
glueContext = GlueContext(SparkContext.getOrCreate())
source = glueContext.create_dynamic_frame.from_options(
    connection_type="s3",
    connection_options={"paths": [args["SOURCE_PATH"]]},
    format="csv",
    format_options={"withHeader": True},
)
glueContext.spark_session.sparkContext.parallelize(
    [str(source.count())]
).saveAsTextFile(args["OUTPUT_PATH"])
"#;

/// The rows of this suite's source data: three records, so what a run writes is a value only
/// a real read of the source produces.
pub(crate) const SOURCE_CSV: &str = "id,name,value\n1,ada,10\n2,bo,20\n3,cy,30\n";

/// Where one case's source objects live.
pub(crate) fn source_prefix(case: &str) -> String {
    format!("s3://{BUCKET}/data/{case}/")
}

/// Where one case's output objects live.
pub(crate) fn output_prefix(case: &str) -> String {
    format!("s3://{BUCKET}/output/{case}")
}

/// The key of the one part file a write leaves under a case's output prefix.
pub(crate) fn part_key(case: &str) -> String {
    format!("output/{case}/part-00000")
}

/// The `CreateJob` body of a Spark ETL job whose script is at `location`.
pub(crate) fn job_body(name: &str, role: &str, location: &str) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "Command": { "Name": "glueetl", "ScriptLocation": location, "PythonVersion": "3" },
        "GlueVersion": "4.0",
        "WorkerType": "G.1X",
        "NumberOfWorkers": 2,
        // The shortest `Timeout` the model admits, well above the span one script may run.
        "Timeout": 1,
    })
}

/// One row: the statements in force on the row's role, and what the run of a job carrying it
/// must come to.
struct Row {
    /// What every assertion of the row reports, and the path segment its objects live under.
    case: &'static str,
    /// The statements in force on the row's role, as (effect, action, resource) — the
    /// resource spelling `BUCKET` for this suite's bucket.
    grants: Vec<(&'static str, &'static str, &'static str)>,
    /// Whether the run must succeed and leave the count of the source's rows behind.
    succeeds: bool,
    /// Whether the failed run's `ErrorMessage` is the missing-path failure of the reader that
    /// resolved no file at the source ([`unresolved_signature`]).
    source_unresolved: bool,
    /// What the failed run's `ErrorMessage` must carry besides that, each of them.
    carries: Vec<&'static str>,
    /// Whether the path the failure names is the row's OUTPUT prefix rather than its source.
    names_the_output: bool,
}

/// The rows: every shape of grant over a source that the sibling role table does not already
/// span, plus the output write beside them.
///
/// The shapes of a grant are spanned here by what decides the SOURCE read: allowed outright,
/// left out of every statement (an implicit deny — the shape of a role given the script read
/// and the output write and never the source read), named by an explicit deny beside an allow
/// of everything, and the listing a source read starts with left out. The output write is one
/// further row, because a refused WRITE is reported differently.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "allowed-outright",
            // "s3:ListBucket" is what a read of a source DIRECTORY needs beside the object
            // reads: the objects under the prefix are listed before they can be read.
            grants: vec![
                ("Allow", "s3:GetObject", "arn:aws:s3:::BUCKET/*"),
                ("Allow", "s3:PutObject", "arn:aws:s3:::BUCKET/*"),
                ("Allow", "s3:ListBucket", "arn:aws:s3:::BUCKET"),
            ],
            succeeds: true,
            source_unresolved: false,
            carries: Vec::new(),
            names_the_output: false,
        },
        Row {
            // A role given the script read and the output write and never the source read:
            // nothing DENIES the source, no statement allows it.
            case: "source-left-out-of-every-allow",
            grants: vec![
                ("Allow", "s3:GetObject", "arn:aws:s3:::BUCKET/scripts/*"),
                ("Allow", "s3:PutObject", "arn:aws:s3:::BUCKET/output/*"),
                ("Allow", "s3:ListBucket", "arn:aws:s3:::BUCKET"),
            ],
            succeeds: false,
            source_unresolved: true,
            carries: Vec::new(),
            names_the_output: false,
        },
        Row {
            case: "source-denied-explicitly",
            grants: vec![
                ("Allow", "s3:*", "*"),
                ("Deny", "s3:GetObject", "arn:aws:s3:::BUCKET/data/*"),
            ],
            succeeds: false,
            source_unresolved: true,
            carries: Vec::new(),
            names_the_output: false,
        },
        Row {
            // The listing a source read starts with is what this role is refused, and a
            // listing the store refuses tells the read nothing about what is under the path.
            case: "source-listing-left-out-of-every-allow",
            grants: vec![
                ("Allow", "s3:GetObject", "arn:aws:s3:::BUCKET/*"),
                ("Allow", "s3:PutObject", "arn:aws:s3:::BUCKET/*"),
            ],
            succeeds: false,
            source_unresolved: true,
            carries: Vec::new(),
            names_the_output: false,
        },
        Row {
            // A refused WRITE names the object it was making, so the store's own refusal is
            // what the run reports.
            case: "output-left-out-of-every-allow",
            grants: vec![
                ("Allow", "s3:GetObject", "arn:aws:s3:::BUCKET/*"),
                ("Allow", "s3:ListBucket", "arn:aws:s3:::BUCKET"),
            ],
            succeeds: false,
            source_unresolved: false,
            carries: vec![ACCESS_DENIED, "403"],
            names_the_output: true,
        },
    ]
}

/// Run one row's job and assert what it came to, with `script` as the job's script.
async fn assert_row(row: &Row, index: usize, script: &str, label: &str) {
    let case = row.case;
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    let role_name = format!("job-source-role-{label}-{index}");
    let role = seed_role(&engine, &role_name, Some(glue_trust()));
    let documents: Vec<Value> = row
        .grants
        .iter()
        .map(|&(effect, action, resource)| {
            statement(effect, &[action], &resource.replace("BUCKET", BUCKET))
        })
        .collect();
    grant_role(&engine, &role_name, &documents, &[]);

    stage_script(&router, &format!("data/{case}/input.csv"), SOURCE_CSV).await;
    let name = format!("job-source-{label}-{case}");
    let location = stage_script(&router, &format!("{SCRIPTS}{name}.py"), script).await;
    ok(&router, "CreateJob", &job_body(&name, &role, &location)).await;

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
    let run = run_at_rest(&router, &engine, case, &name, &run_id).await;
    let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
    let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
    let written = object_body(&router, &part_key(case)).await;

    if row.succeeds {
        assert_eq!(
            state, JOB_RUN_STATE_SUCCEEDED,
            "case {case:?} ({label}): expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
        );
        assert_eq!(
            written.as_deref(),
            Some("3\n"),
            "case {case:?} ({label}): expected the count of the source's rows, got {written:?}"
        );
        return;
    }
    assert_eq!(
        state, JOB_RUN_STATE_FAILED,
        "case {case:?} ({label}): expected {JOB_RUN_STATE_FAILED}, got {run}"
    );
    for held in &row.carries {
        assert!(
            message.contains(held),
            "case {case:?} ({label}): expected an ErrorMessage carrying {held:?}, got \
             {message:?}"
        );
    }
    // A source the read resolved no file at is reported in the words of the READER that
    // resolved it, which is the one thing the two readers of the same condition differ in.
    if row.source_unresolved {
        let signature = unresolved_signature(label);
        assert!(
            message.contains(signature),
            "case {case:?} ({label}): expected an ErrorMessage carrying {signature:?}, got \
             {message:?}"
        );
    }
    // The path the run could not reach is named, which is the one variable part of the
    // failure.
    let named = if row.names_the_output {
        output_prefix(case)
    } else {
        source_prefix(case)
    };
    assert!(
        message.contains(&named),
        "case {case:?} ({label}): expected the ErrorMessage to name {named:?}, got {message:?}"
    );
    assert!(
        written.is_none(),
        "case {case:?} ({label}): a run that was refused wrote no output, got {written:?}"
    );
}

/// A source path the role's policies do not allow is refused the run exactly as one they deny
/// is, and the run reports it as a path that is not there.
///
/// Both readers a Glue job script has are driven: the whole table through the Spark dataset
/// reader, and the two ends of it — the source allowed, the source left out of every allow —
/// through the AWS Glue ETL library's own source reader, which is what tells that reader's own
/// handling of a refusal apart. One test, so the rows run one at a time: each one executes a
/// script, and the interpreters this emulator runs scripts on are shared by every suite.
#[tokio::test]
async fn a_source_its_role_may_not_read_is_reported_as_a_path_that_is_not_there() {
    for (index, row) in rows().iter().enumerate() {
        assert_row(row, index, COUNTS_ITS_SOURCE, "dataset").await;
    }
    let ends = ["allowed-outright", "source-left-out-of-every-allow"];
    for (index, row) in rows()
        .iter()
        .enumerate()
        .filter(|(_index, row)| ends.contains(&row.case))
    {
        assert_row(row, index, COUNTS_ITS_SOURCE_THROUGH_A_FRAME, FRAME_READER).await;
    }
}
