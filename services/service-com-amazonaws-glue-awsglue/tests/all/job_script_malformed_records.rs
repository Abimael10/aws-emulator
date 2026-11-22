// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Glue job run does with a source RECORD IT CANNOT READ under
//! the schema the script gave the read.
//!
//! The rule under test: a read made under an explicit schema reads every value as the type
//! its own column declares, and a record holding a value that type cannot be read from is
//! treated as the read's `mode` option says — the default keeps the record with that field as
//! a null, `DROPMALFORMED` leaves the record out, and `FAILFAST` fails the read, so the run
//! that made it ends in the modelled failure state carrying the engine's own reason. A field
//! the source holds NOTHING in is a null rather than a malformed one, whatever the mode.
//!
//! The rows span what the rule generalises over: WHICH mode is in force (each of the three,
//! and one no engine knows), and WHAT the source holds (a value that cannot be read, an empty
//! cell, and a source every value of which reads).
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, the scripts and
//! their sources are staged through Amazon S3's own `PutObject`, and each run is read back
//! until it has come to rest ([`crate::job_runs::run_at_rest`]) with the span the clock owes
//! it made due through the store rather than slept out.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};

use crate::job_script_execution::{
    create_bucket, emulator_with_s3, object_body, run_to_rest, stage_script, BUCKET, ROLE_ARN,
    SCRIPTS,
};
use crate::wire::ok;

/// A script that reads its CSV source under an explicit schema, in the mode its `--MODE`
/// argument names, and writes out how many records the read answered and what each of them
/// held — so what the read really took in is readable from the object store.
const READS_UNDER_A_SCHEMA: &str = r#"
import sys

from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext
from pyspark.sql import SparkSession

args = getResolvedOptions(sys.argv, ["SOURCE_PATH", "OUTPUT_PATH", "MODE"])
spark = SparkSession(SparkContext.getOrCreate())
source = (
    spark.read.schema("id bigint, region string, amount double")
    .option("header", "true")
    .option("mode", args["MODE"])
    .csv(args["SOURCE_PATH"])
)
lines = ["rows=%d" % (source.count(),)]
for row in source.collect():
    lines.append("%s|%s|%s" % (row["id"], row["region"], row["amount"]))
spark.sparkContext.parallelize(lines).saveAsTextFile(args["OUTPUT_PATH"])
"#;

/// A source holding one value the declared `double` cannot be read from, and one cell holding
/// nothing at all.
const SOURCE_WITH_A_MALFORMED_ROW: &str = "id,region,amount\n\
                                           1,west,10.5\n\
                                           2,east,not-a-number\n\
                                           3,south,\n";

/// The same shape of source, every value of which reads under the declared schema.
const SOURCE_THAT_READS: &str = "id,region,amount\n\
                                 1,west,10.5\n\
                                 2,east,3.25\n";

/// The whole reason a read in the failing-fast mode gives for the record it could not read:
/// the record itself, the mode in force, and the mode that would have read it as a null.
const MALFORMED_RECORD: &str = "SparkException: Malformed records are detected in record \
                                parsing: [2,east,not-a-number]. Parse Mode: FAILFAST. To \
                                process malformed records as null result, try setting the \
                                option 'mode' as 'PERMISSIVE'.";

/// The `Timeout` every job here carries, in minutes: the shortest the model admits.
const TIMEOUT_MINUTES: i64 = 1;

/// One row: the source a run reads, the mode it reads it in, and what that run must come to.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The job's name, which is also the path segment its objects live under.
    label: &'static str,
    /// The CSV staged under the row's source prefix.
    source: &'static str,
    /// The `mode` option the read is made with.
    mode: &'static str,
    /// The body the run's one output part file must carry, or `None` for a run that writes
    /// nothing because its read failed.
    written: Option<&'static str>,
    /// The `ErrorMessage` the failed run must carry, exactly.
    reason: Option<&'static str>,
}

/// RULE: a record the declared schema cannot be read over is treated as the read's own mode
/// says, and an empty cell is a null in every mode.
#[tokio::test]
async fn a_record_the_schema_cannot_read_is_treated_as_the_reads_mode_says() {
    let rows = [
        Row {
            case: "the default mode over a source holding a value it cannot read",
            label: "read-permissive",
            source: SOURCE_WITH_A_MALFORMED_ROW,
            mode: "PERMISSIVE",
            written: Some("rows=3\n1|west|10.5\n2|east|None\n3|south|None\n"),
            reason: None,
        },
        Row {
            case: "the dropping mode over the same source",
            label: "read-dropmalformed",
            source: SOURCE_WITH_A_MALFORMED_ROW,
            mode: "DROPMALFORMED",
            written: Some("rows=2\n1|west|10.5\n3|south|None\n"),
            reason: None,
        },
        Row {
            case: "the failing-fast mode over the same source",
            label: "read-failfast",
            source: SOURCE_WITH_A_MALFORMED_ROW,
            mode: "FAILFAST",
            written: None,
            reason: Some(MALFORMED_RECORD),
        },
        Row {
            case: "a mode no engine knows, which is read in the default one",
            label: "read-unknown-mode",
            source: SOURCE_WITH_A_MALFORMED_ROW,
            mode: "STRICTLY",
            written: Some("rows=3\n1|west|10.5\n2|east|None\n3|south|None\n"),
            reason: None,
        },
        Row {
            case: "the failing-fast mode over a source every value of which reads",
            label: "read-failfast-clean",
            source: SOURCE_THAT_READS,
            mode: "FAILFAST",
            written: Some("rows=2\n1|west|10.5\n2|east|3.25\n"),
            reason: None,
        },
    ];

    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for row in &rows {
        let case = row.case;
        let label = row.label;
        stage_script(&router, &format!("data/{label}/input.csv"), row.source).await;
        let location =
            stage_script(&router, &format!("{SCRIPTS}{label}.py"), READS_UNDER_A_SCHEMA).await;
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": label,
                "Role": ROLE_ARN,
                "Command": {
                    "Name": "glueetl",
                    "ScriptLocation": location,
                    "PythonVersion": "3",
                },
                "GlueVersion": "4.0",
                "WorkerType": "G.1X",
                "NumberOfWorkers": 2,
                "Timeout": TIMEOUT_MINUTES,
            }),
        )
        .await;

        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--SOURCE_PATH": format!("s3://{BUCKET}/data/{label}/"),
                "--OUTPUT_PATH": format!("s3://{BUCKET}/output/{label}"),
                "--MODE": row.mode,
            }),
        )
        .await;

        let expected_state = if row.reason.is_some() {
            JOB_RUN_STATE_FAILED
        } else {
            JOB_RUN_STATE_SUCCEEDED
        };
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(expected_state),
            "case {case:?}: expected the run to come to rest in {expected_state}, got {run}"
        );
        assert_eq!(
            run.get("ErrorMessage").and_then(Value::as_str),
            row.reason,
            "case {case:?}: expected the reason {:?}, got {run}",
            row.reason
        );
        let written = object_body(&router, &format!("output/{label}/part-00000")).await;
        assert_eq!(
            written.as_deref(),
            row.written,
            "case {case:?}: expected the run to leave {:?}, got {written:?}",
            row.written
        );
    }
}
