// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WRITE OPTIONS a Glue ETL script's sink is given.
//!
//! The rule under test: a sink PERFORMS the connection options the service documents for a
//! write to Amazon S3 — where it writes, the keys it partitions by, the `compression` its
//! parts are stored under, and whether (`enableUpdateCatalog`) and how (`updateBehavior`)
//! what it wrote is brought into the Data Catalog — so a write naming one gets the write it
//! asked for rather than a refusal, and a write naming a value this emulator cannot perform
//! fails the run naming it instead of leaving behind an object that is not what was asked
//! for.
//!
//! So the codec a write names is judged with a PARQUET READER (the `parquet` crate) on the
//! object the run really left, the partitions it registered are read back through
//! `GetPartitions`, and the registered schema through `GetTable`: nothing here is asserted
//! from the run record alone.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: WHICH side
//! names the codec (the write's connection options, the layout's format options, both, or
//! neither), WHICH codec it names (one the parts are really stored under, none at all, and
//! one this emulator cannot store a part under), and WHAT the write tells the catalog to do
//! with the schema it wrote (update it, log the difference, name neither).
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, the scripts and
//! the data they read are staged through Amazon S3's own `PutObject`, and each run is read
//! back until it has come to rest ([`crate::job_runs::run_at_rest`]) with the span the clock
//! owes it made due through the store rather than slept out — so no outcome here depends on
//! how fast the host runs.

use parquet::basic::Compression;
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_script_catalog_read::{emulator_with_catalog_and_s3, job_role};
use crate::job_script_execution::{
    create_bucket, emulator_with_s3, run_to_rest, stage_script, BUCKET, ROLE_ARN, SCRIPTS,
};
// The object readers and the Parquet read of a written part are the ones the columnar-output
// suite already owns ([`crate::job_script_parquet_output`]), so what a part holds is judged
// here by the same reader.
use crate::job_script_parquet_output::{held_by, keys_under, object_bytes};
use crate::wire::ok;

/// The `GlueVersion` and worker shape every job here runs under.
const GLUE_VERSION: &str = "4.0";

/// The `Timeout` every job here carries, in minutes: the shortest the model admits, since
/// the span one script run may last is this emulator's own ceiling and far below it.
const TIMEOUT_MINUTES: i64 = 1;

/// The source every row reads: two rows over two values of the field a write partitions by.
const SOURCE: &str = "id,city,amount\n1,Berlin,10.5\n2,Lisbon,20\n";

/// A script that writes its frame through the documented sink, naming the codec its parts
/// are stored under on whichever side its arguments say — the write's own connection
/// options, the layout's format options, or neither (`-`).
const WRITES_UNDER_A_CODEC: &str = r#"
import sys

from awsglue.context import GlueContext
from awsglue.job import Job
from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext

args = getResolvedOptions(
    sys.argv,
    [
        "JOB_NAME",
        "source_path",
        "output_path",
        "layout",
        "connection_compression",
        "format_compression",
    ],
)
glue_context = GlueContext(SparkContext())
job = Job(glue_context)
job.init(args["JOB_NAME"], args)
frame = glue_context.create_dynamic_frame.from_options(
    connection_type="s3",
    connection_options={"paths": [args["source_path"]]},
    format="csv",
    format_options={"withHeader": True},
)
options = {"path": args["output_path"]}
if args["connection_compression"] != "-":
    options["compression"] = args["connection_compression"]
sink = glue_context.getSink(connection_type="s3", **options)
if args["format_compression"] != "-":
    sink.setFormat(args["layout"], compression=args["format_compression"])
else:
    sink.setFormat(args["layout"])
sink.writeFrame(frame)
job.commit()
"#;

/// A script that writes its frame into a REGISTERED table: partitioned, asked to update the
/// catalog, and following whichever update behaviour its arguments name (`-` names none).
const WRITES_INTO_THE_CATALOG: &str = r#"
import sys

from awsglue.context import GlueContext
from awsglue.job import Job
from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext

args = getResolvedOptions(
    sys.argv,
    ["JOB_NAME", "source_path", "output_path", "database", "table", "behaviour"],
)
glue_context = GlueContext(SparkContext())
job = Job(glue_context)
job.init(args["JOB_NAME"], args)
frame = glue_context.create_dynamic_frame.from_options(
    connection_type="s3",
    connection_options={"paths": [args["source_path"]]},
    format="csv",
    format_options={"withHeader": True},
)
options = {
    "path": args["output_path"],
    "partitionKeys": ["city"],
    "compression": "snappy",
    "enableUpdateCatalog": True,
}
if args["behaviour"] != "-":
    options["updateBehavior"] = args["behaviour"]
sink = glue_context.getSink(connection_type="s3", **options)
sink.setCatalogInfo(catalogDatabase=args["database"], catalogTableName=args["table"])
sink.setFormat("glueparquet")
sink.writeFrame(frame)
job.commit()
"#;

/// The `CreateJob` body of a Spark job whose script is at `location`, running as `role`.
fn job_body(name: &str, location: &str, role: &str) -> Value {
    json!({
        "Name": name,
        "Role": role,
        "Command": { "Name": "glueetl", "ScriptLocation": location, "PythonVersion": "3" },
        "GlueVersion": GLUE_VERSION,
        "WorkerType": "G.1X",
        "NumberOfWorkers": 2,
        "Timeout": TIMEOUT_MINUTES,
    })
}

/// The rows a write of [`SOURCE`] leaves in its one part file: the text's own values, which
/// is what a dynamic frame read out of delimited text holds.
fn written_rows() -> Vec<Vec<String>> {
    vec![
        vec!["1".to_string(), "Berlin".to_string(), "10.5".to_string()],
        vec!["2".to_string(), "Lisbon".to_string(), "20".to_string()],
    ]
}

/// RULE: `compression` is a write option of the sink as well as a format option of the
/// layout, and names the same codec in both — so the parts a write leaves are really stored
/// under the codec it named, and the part file carries that codec's own suffix.
#[tokio::test]
async fn the_codec_a_write_names_is_the_one_its_parts_are_stored_under() {
    // (case, label, the codec the connection options name, the one the format options name,
    //  the part file the write must leave, the codec its pages must then be stored under)
    let rows: [(&str, &str, &str, &str, &str, Compression); 4] = [
        (
            "the codec named among the write's own connection options",
            "codec-connection",
            "snappy",
            "-",
            "part-00000.snappy.parquet",
            Compression::SNAPPY,
        ),
        (
            "no compression named at all, so the layout's default",
            "codec-default",
            "-",
            "-",
            "part-00000.snappy.parquet",
            Compression::SNAPPY,
        ),
        (
            "no compression at all, named among the connection options",
            "codec-uncompressed",
            "uncompressed",
            "-",
            "part-00000.parquet",
            Compression::UNCOMPRESSED,
        ),
        (
            "both sides naming one, where the layout's own decides",
            "codec-both",
            "snappy",
            "none",
            "part-00000.parquet",
            Compression::UNCOMPRESSED,
        ),
    ];

    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (case, label, connection, format, part, stored) in rows {
        stage_script(&router, &format!("data/{label}/input.csv"), SOURCE).await;
        let location =
            stage_script(&router, &format!("{SCRIPTS}{label}.py"), WRITES_UNDER_A_CODEC).await;
        ok(&router, "CreateJob", &job_body(label, &location, ROLE_ARN)).await;

        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--JOB_NAME": label,
                "--source_path": format!("s3://{BUCKET}/data/{label}/"),
                "--output_path": format!("s3://{BUCKET}/output/{label}"),
                "--layout": "glueparquet",
                "--connection_compression": connection,
                "--format_compression": format,
            }),
        )
        .await;
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: the write performs the option it was given, so the run \
             succeeds, got {run}"
        );

        let prefix = format!("output/{label}/");
        let listed = keys_under(&router, case, &prefix).await;
        let parts: Vec<String> = listed
            .iter()
            .map(|key| key[prefix.len()..].to_string())
            .collect();
        assert_eq!(
            parts,
            vec![part.to_string()],
            "case {case:?}: expected the part file {part:?} under {prefix} and nothing \
             else, got {listed:?}"
        );

        let key = format!("{prefix}{part}");
        let body = object_bytes(&router, &key)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: {key} must be served"));
        let held = held_by(case, &key, &body);
        assert_eq!(
            held.compression,
            format!("{stored:?}"),
            "case {case:?}: expected the pages of {key} stored as {stored:?}, got {:?}",
            held.compression
        );
        assert_eq!(
            held.rows,
            written_rows(),
            "case {case:?}: expected {key} to hold the frame's own records, got {:?}",
            held.rows
        );
    }
}

/// RULE: a codec this emulator cannot store a part under is refused NAMING it, before
/// anything is written — a write reported as done under a codec nothing applied would be a
/// wrong answer reported as a success.
#[tokio::test]
async fn a_codec_no_part_can_be_stored_under_fails_the_run_naming_it() {
    // (case, label, the layout written, the codec the connection options name)
    let rows: [(&str, &str, &str, &str); 3] = [
        (
            "a codec the columnar writer cannot encode a page with",
            "codec-parquet-gzip",
            "glueparquet",
            "gzip",
        ),
        (
            "a codec named for a write of delimited text",
            "codec-csv-gzip",
            "csv",
            "gzip",
        ),
        (
            "a codec named for a write of JSON documents",
            "codec-json-gzip",
            "json",
            "gzip",
        ),
    ];

    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;

    for (case, label, layout, codec) in rows {
        stage_script(&router, &format!("data/{label}/input.csv"), SOURCE).await;
        let location =
            stage_script(&router, &format!("{SCRIPTS}{label}.py"), WRITES_UNDER_A_CODEC).await;
        ok(&router, "CreateJob", &job_body(label, &location, ROLE_ARN)).await;

        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--JOB_NAME": label,
                "--source_path": format!("s3://{BUCKET}/data/{label}/"),
                "--output_path": format!("s3://{BUCKET}/output/{label}"),
                "--layout": layout,
                "--connection_compression": codec,
                "--format_compression": "-",
            }),
        )
        .await;
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_FAILED),
            "case {case:?}: the write was refused, so the run fails, got {run}"
        );
        let message = run
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains(codec),
            "case {case:?}: expected the script's own error naming {codec:?}, got \
             {message:?}"
        );
        assert!(
            keys_under(&router, case, &format!("output/{label}/"))
                .await
                .is_empty(),
            "case {case:?}: nothing was written, so the output prefix holds no object"
        );
    }
}

/// RULE: a write APPENDS to the path it is given — the objects an earlier write left there
/// are data of the same dataset, so a later write of that path leaves its own part beside
/// them instead of replacing one.
#[tokio::test]
async fn a_second_write_of_one_path_leaves_its_part_beside_the_first() {
    let case = "the same write run twice over one path";
    let label = "codec-appended";

    let (router, engine) = emulator_with_s3();
    create_bucket(&router).await;
    stage_script(&router, &format!("data/{label}/input.csv"), SOURCE).await;
    let location =
        stage_script(&router, &format!("{SCRIPTS}{label}.py"), WRITES_UNDER_A_CODEC).await;
    ok(&router, "CreateJob", &job_body(label, &location, ROLE_ARN)).await;

    let prefix = format!("output/{label}/");
    let mut first: Option<Vec<u8>> = None;
    // (which run this is, the parts the path must hold after it)
    for (turn, expected) in [
        (1, vec!["part-00000.snappy.parquet"]),
        (
            2,
            vec!["part-00000.snappy.parquet", "part-00001.snappy.parquet"],
        ),
    ] {
        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--JOB_NAME": label,
                "--source_path": format!("s3://{BUCKET}/data/{label}/"),
                "--output_path": format!("s3://{BUCKET}/output/{label}"),
                "--layout": "glueparquet",
                "--connection_compression": "snappy",
                "--format_compression": "-",
            }),
        )
        .await;
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: run {turn} writes its frame out, so it succeeds, got {run}"
        );

        let listed = keys_under(&router, case, &prefix).await;
        let parts: Vec<String> = listed
            .iter()
            .map(|key| key[prefix.len()..].to_string())
            .collect();
        assert_eq!(
            parts,
            expected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {case:?}: after run {turn} the path holds {expected:?}, got {listed:?}"
        );

        // What the FIRST write left is untouched by the second: the same bytes, holding the
        // same records.
        let key = format!("{prefix}part-00000.snappy.parquet");
        let body = object_bytes(&router, &key)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: {key} must be served after run {turn}"));
        let held = held_by(case, &key, &body);
        assert_eq!(
            held.rows,
            written_rows(),
            "case {case:?}: after run {turn} the first part still holds its own records, \
             got {:?}",
            held.rows
        );
        match &first {
            None => first = Some(body),
            Some(before) => assert_eq!(
                before, &body,
                "case {case:?}: the second write leaves the first part's bytes as they were"
            ),
        }
    }
}

/// A script that reads a REGISTERED table and writes what it read out — through the sink or
/// by converting the frame to a dataset of rows first, as its `--through` argument says.
const WRITES_WHAT_IT_READ: &str = r#"
import sys

from awsglue.context import GlueContext
from awsglue.job import Job
from awsglue.utils import getResolvedOptions
from pyspark.context import SparkContext

args = getResolvedOptions(
    sys.argv, ["JOB_NAME", "database", "table", "output_path", "through"]
)
glue_context = GlueContext(SparkContext())
job = Job(glue_context)
job.init(args["JOB_NAME"], args)
frame = glue_context.create_dynamic_frame.from_catalog(
    database=args["database"], table_name=args["table"]
)
if args["through"] == "dataset":
    frame.toDF().write.mode("overwrite").parquet(args["output_path"])
else:
    glue_context.write_dynamic_frame.from_options(
        frame=frame,
        connection_type="s3",
        connection_options={"path": args["output_path"]},
        format="parquet",
    )
job.commit()
"#;

/// Create the database and a table declaring typed columns over the objects at `prefix`,
/// with `amount` declared as `declared` — the type whose own values the read resolves a cell
/// against.
async fn create_typed_source(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    prefix: &str,
    declared: &str,
) {
    let (status, body) = crate::wire::call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert!(
        status == 200 || crate::wire::error_code(&body).contains("AlreadyExistsException"),
        "seeding {database}: expected the database to exist, got {status} with {body}"
    );
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{prefix}"),
                    "Columns": [
                        { "Name": "id", "Type": "int" },
                        { "Name": "city", "Type": "string" },
                        { "Name": "amount", "Type": declared },
                    ],
                    "SerdeInfo": { "Parameters": { "field.delim": "," } },
                },
            },
        }),
    )
    .await;
}

/// One row of the declared-type table: a source read under a column type, written one way,
/// and whether the run of it must succeed.
struct Typed {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The job's name, which is also the path segment and the table its objects belong to.
    label: &'static str,
    /// The objects staged under the table's location.
    source: &'static str,
    /// The type the table declares for `amount`, in Hive type syntax — a parameter list does
    /// not change WHICH type is named (`Column$Type`).
    declared: &'static str,
    /// How the frame leaves the run: through the sink, or converted to a dataset of rows.
    through: &'static str,
    /// Whether the run must come to rest having written its frame out.
    succeeds: bool,
}

/// RULE: a frame carries the value its source really held, so a cell the column's declared
/// type could not be read from leaves that column holding a value that is no value of its
/// own type — and every operation needing ONE type for a column (writing the frame out,
/// converting it to a dataset of rows) is refused NAMING that column instead of storing text
/// where the schema states a number. A declaration's parameter list does not change which
/// type it names, so a value of a parameterised numeric type is one of its column's own.
#[tokio::test]
async fn a_value_outside_its_columns_declared_type_is_refused_naming_the_column() {
    let rows = [
        Typed {
            case: "a frame every value of which is one of its column's type, written out",
            label: "typed-clean-sink",
            source: "1,Berlin,10.5\n2,Lisbon,20\n",
            declared: "double",
            through: "sink",
            succeeds: true,
        },
        Typed {
            case: "a frame holding a cell the declared type could not be read from, written \
                   out",
            label: "typed-violating-sink",
            source: "1,Berlin,10.5\n2,Lisbon,not-a-number\n",
            declared: "double",
            through: "sink",
            succeeds: false,
        },
        Typed {
            case: "the same clean frame under a parameterised numeric type",
            label: "typed-clean-decimal",
            source: "1,Berlin,10.5\n2,Lisbon,20\n",
            declared: "decimal(10,2)",
            through: "sink",
            succeeds: true,
        },
        Typed {
            case: "a cell that is no number under a parameterised numeric type",
            label: "typed-violating-decimal",
            source: "1,Berlin,10.5\n2,Lisbon,not-a-number\n",
            declared: "decimal(10,2)",
            through: "sink",
            succeeds: false,
        },
        Typed {
            case: "the same cell under a parameterised TEXT type, whose values it is one of",
            label: "typed-clean-varchar",
            source: "1,Berlin,10.5\n2,Lisbon,not-a-number\n",
            declared: "varchar(32)",
            through: "sink",
            succeeds: true,
        },
        Typed {
            case: "the clean frame, converted to a dataset of rows first",
            label: "typed-clean-dataset",
            source: "1,Berlin,10.5\n2,Lisbon,20\n",
            declared: "double",
            through: "dataset",
            succeeds: true,
        },
        Typed {
            case: "the frame holding that cell, converted to a dataset of rows first",
            label: "typed-violating-dataset",
            source: "1,Berlin,10.5\n2,Lisbon,not-a-number\n",
            declared: "double",
            through: "dataset",
            succeeds: false,
        },
    ];

    let (router, engine) = emulator_with_catalog_and_s3();
    create_bucket(&router).await;
    let role = job_role(&engine, "typed-source-role");

    for row in rows {
        let (case, label, through, succeeds) = (row.case, row.label, row.through, row.succeeds);
        let database = "db_typed_source";
        let table = &label.replace('-', "_");
        let prefix = format!("data/{label}/");
        stage_script(&router, &format!("{prefix}part-0.csv"), row.source).await;
        create_typed_source(&router, database, table, &prefix, row.declared).await;
        let location = stage_script(
            &router,
            &format!("{SCRIPTS}{label}.py"),
            WRITES_WHAT_IT_READ,
        )
        .await;
        ok(&router, "CreateJob", &job_body(label, &location, &role)).await;

        let output = format!("output/{label}");
        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--JOB_NAME": label,
                "--database": database,
                "--table": table,
                "--output_path": format!("s3://{BUCKET}/{output}"),
                "--through": through,
            }),
        )
        .await;

        if succeeds {
            assert_eq!(
                run.get("JobRunState").and_then(Value::as_str),
                Some(JOB_RUN_STATE_SUCCEEDED),
                "case {case:?}: every value is one of its column's type, so the run \
                 succeeds, got {run}"
            );
            assert!(
                !keys_under(&router, case, &format!("{output}/")).await.is_empty(),
                "case {case:?}: the run wrote its frame out, so the path holds its part"
            );
            continue;
        }
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_FAILED),
            "case {case:?}: the column has no one type, so the run fails, got {run}"
        );
        let message = run
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains("amount"),
            "case {case:?}: expected the script's own error naming the column \"amount\", \
             got {message:?}"
        );
        assert!(
            keys_under(&router, case, &format!("{output}/"))
                .await
                .is_empty(),
            "case {case:?}: nothing was written, so the output path holds no object"
        );
    }
}

/// Create the database and the partitioned table one row of the catalog suite writes into,
/// registered with ONE column — a schema the frame the run writes does not have, so what the
/// update behaviour does to it is observable.
async fn create_target(router: &MultiServiceRouter, database: &str, table: &str, prefix: &str) {
    let (status, body) = crate::wire::call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert!(
        status == 200 || crate::wire::error_code(&body).contains("AlreadyExistsException"),
        "seeding {database}: expected the database to exist, got {status} with {body}"
    );
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "Parameters": { "classification": "parquet" },
                "PartitionKeys": [{ "Name": "city", "Type": "string" }],
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{prefix}"),
                    "Columns": [{ "Name": "id", "Type": "string" }],
                },
            },
        }),
    )
    .await;
}

/// The `(Name, Type)` of each column the catalog holds for `table`, in declaration order.
async fn registered_columns(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
) -> Vec<(String, String)> {
    let read = ok(
        router,
        "GetTable",
        &json!({ "DatabaseName": database, "Name": table }),
    )
    .await;
    read["Table"]["StorageDescriptor"]["Columns"]
        .as_array()
        .unwrap_or_else(|| panic!("GetTable must report the table's columns, got {read}"))
        .iter()
        .map(|column| {
            (
                column["Name"].as_str().unwrap_or_default().to_string(),
                column["Type"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The `(first value, location)` of each partition the catalog holds for `table`, ordered.
async fn registered_partitions(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
) -> Vec<(String, String)> {
    let listed = ok(
        router,
        "GetPartitions",
        &json!({ "DatabaseName": database, "TableName": table }),
    )
    .await;
    let mut held: Vec<(String, String)> = listed["Partitions"]
        .as_array()
        .unwrap_or_else(|| panic!("GetPartitions must answer a list, got {listed}"))
        .iter()
        .map(|partition| {
            (
                partition["Values"][0]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                partition["StorageDescriptor"]["Location"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    held.sort();
    held
}

/// One row of the update-behaviour table: a write naming a behaviour, and what the catalog
/// must then hold for the table it registered into.
struct Behaviour {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The job's name, which is also the path segment and the table its objects belong to.
    label: &'static str,
    /// The `updateBehavior` the write names, or `-` for a write naming none.
    names: &'static str,
    /// The `(Name, Type)` of each column the catalog must hold for the table afterwards.
    columns: &'static [(&'static str, &'static str)],
}

/// RULE: a write asked to update the catalog registers the partitions it laid down either
/// way, and `updateBehavior` decides what it does to the registered TABLE's schema: the one
/// the frame was written under replaces it, or the difference is only reported and the
/// registered schema left as it stands.
#[tokio::test]
async fn the_update_behaviour_a_write_names_decides_what_the_catalog_schema_becomes() {
    let rows = [
        Behaviour {
            case: "the write naming that the registered schema is updated",
            label: "catalog-update",
            names: "UPDATE_IN_DATABASE",
            columns: &[("id", "string"), ("amount", "string")],
        },
        Behaviour {
            case: "the write naming no behaviour at all",
            label: "catalog-default",
            names: "-",
            columns: &[("id", "string"), ("amount", "string")],
        },
        Behaviour {
            case: "the write naming that the difference is only logged",
            label: "catalog-log",
            names: "LOG",
            columns: &[("id", "string")],
        },
    ];

    let (router, engine) = emulator_with_catalog_and_s3();
    create_bucket(&router).await;
    let role = job_role(&engine, "sink-options-role");

    for row in rows {
        let (case, label, behaviour, expected_columns) =
            (row.case, row.label, row.names, row.columns);
        let database = "db_sink_options";
        let table = &label.replace('-', "_");
        let prefix = format!("written/{label}/");
        create_target(&router, database, table, &prefix).await;
        stage_script(&router, &format!("data/{label}/input.csv"), SOURCE).await;
        let location = stage_script(
            &router,
            &format!("{SCRIPTS}{label}.py"),
            WRITES_INTO_THE_CATALOG,
        )
        .await;
        ok(&router, "CreateJob", &job_body(label, &location, &role)).await;

        let run = run_to_rest(
            &router,
            &engine,
            label,
            &json!({
                "--JOB_NAME": label,
                "--source_path": format!("s3://{BUCKET}/data/{label}/"),
                "--output_path": format!("s3://{BUCKET}/{prefix}"),
                "--database": database,
                "--table": table,
                "--behaviour": behaviour,
            }),
        )
        .await;
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: the write performs the options it was given, so the run \
             succeeds, got {run}"
        );

        // One Parquet part per value of the partition key, stored under the codec the
        // write's own connection options named.
        let listed = keys_under(&router, case, &prefix).await;
        let parts: Vec<String> = listed
            .iter()
            .map(|key| key[prefix.len()..].to_string())
            .collect();
        assert_eq!(
            parts,
            vec![
                "city=Berlin/part-00000.snappy.parquet".to_string(),
                "city=Lisbon/part-00000.snappy.parquet".to_string(),
            ],
            "case {case:?}: expected one Parquet part per partition value, got {listed:?}"
        );

        // The partitions the write registered: one per value written, at the location its
        // own objects are under — whichever update behaviour the write named.
        assert_eq!(
            registered_partitions(&router, database, table).await,
            vec![
                (
                    "Berlin".to_string(),
                    format!("s3://{BUCKET}/{prefix}city=Berlin")
                ),
                (
                    "Lisbon".to_string(),
                    format!("s3://{BUCKET}/{prefix}city=Lisbon")
                ),
            ],
            "case {case:?}: expected the partitions the write registered"
        );

        let held = registered_columns(&router, database, table).await;
        assert_eq!(
            held,
            expected_columns
                .iter()
                .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
                .collect::<Vec<_>>(),
            "case {case:?}: expected the catalog to hold the columns {expected_columns:?}, \
             got {held:?}"
        );
    }
}

/// RULE: an `updateBehavior` that is neither of the two the write option names is refused
/// naming it, before anything is written or registered.
#[tokio::test]
async fn an_update_behaviour_that_is_neither_fails_the_run_naming_it() {
    let case = "a write naming an update behaviour the option does not take";
    let label = "catalog-unknown";
    let behaviour = "REPLACE_EVERYTHING";

    let (router, engine) = emulator_with_catalog_and_s3();
    create_bucket(&router).await;
    let role = job_role(&engine, "sink-behaviour-role");
    let database = "db_sink_behaviour";
    let table = "t_sink_behaviour";
    let prefix = format!("written/{label}/");
    create_target(&router, database, table, &prefix).await;
    stage_script(&router, &format!("data/{label}/input.csv"), SOURCE).await;
    let location = stage_script(
        &router,
        &format!("{SCRIPTS}{label}.py"),
        WRITES_INTO_THE_CATALOG,
    )
    .await;
    ok(&router, "CreateJob", &job_body(label, &location, &role)).await;

    let run = run_to_rest(
        &router,
        &engine,
        label,
        &json!({
            "--JOB_NAME": label,
            "--source_path": format!("s3://{BUCKET}/data/{label}/"),
            "--output_path": format!("s3://{BUCKET}/{prefix}"),
            "--database": database,
            "--table": table,
            "--behaviour": behaviour,
        }),
    )
    .await;
    assert_eq!(
        run.get("JobRunState").and_then(Value::as_str),
        Some(JOB_RUN_STATE_FAILED),
        "case {case:?}: the write was refused, so the run fails, got {run}"
    );
    let message = run
        .get("ErrorMessage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(behaviour),
        "case {case:?}: expected the script's own error naming {behaviour:?}, got {message:?}"
    );
    assert!(
        keys_under(&router, case, &prefix).await.is_empty(),
        "case {case:?}: nothing was written, so the write's path holds no object"
    );
    assert!(
        registered_partitions(&router, database, table)
            .await
            .is_empty(),
        "case {case:?}: nothing was written, so the catalog holds no partition"
    );
    let held = registered_columns(&router, database, table).await;
    assert_eq!(
        held,
        vec![("id".to_string(), "string".to_string())],
        "case {case:?}: nothing was registered, so the table's schema stands as created, \
         got {held:?}"
    );
}
