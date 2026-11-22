// SPDX-License-Identifier: CC0-1.0

//! What a snapshot EXPORT writes for the SERVER's own stored schema, and how an exported
//! column of instants is typed.
//!
//! The rules this suite pins:
//!
//! * An export writes one Apache Parquet object per table the source's STORAGE holds,
//!   whoever wrote it — a MySQL-family server keeps its grant tables and its help tables in
//!   the `mysql` schema on disk, so a snapshot of an instance no client has ever connected to
//!   carries them and the export of that snapshot writes each one out under
//!   `<ExportTaskIdentifier>/mysql/mysql.<table>/`, with one entry per table in the file that
//!   says what was exported. The two schemas a server MATERIALISES from its own state
//!   (`information_schema`, `performance_schema`) hold nothing on disk, so nothing is written
//!   for either.
//! * A column the source declared an INSTANT (`DATETIME`, `TIMESTAMP`) is exported as a
//!   64-bit Parquet TIMESTAMP in microseconds carrying the instant each of its values denotes,
//!   rather than as the text the engine stores it as; a column declared an instant whose
//!   values denote none, and a column declared any other temporal type, keep the values they
//!   hold.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, and every object asserted here is read back through Amazon
//! S3's own `ListObjectsV2` and `GetObject` — so what this suite finds is what that service
//! really holds, read with the Apache Parquet reader.

use bytes::Bytes;
use parquet::basic::{ConvertedType, Type as PhysicalType};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use serde_json::Value;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{available_instance, mysql_login, MySqlSession, Transport};
use crate::db_instances::{call, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role, s3};

/// The canonical account id every generated emulator presents, for the ARN an export names
/// its source by.
const ACCOUNT: &str = "000000000000";

/// The master user every instance below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Everything one export needs beside its source: the destination bucket, the role that may
/// write it and the key it is encrypted under.
struct Destination {
    bucket: String,
    role_arn: String,
    key_id: String,
}

/// Create that destination, through each owning service's own operations.
async fn destination(router: &MultiServiceRouter, slug: &str) -> Destination {
    let bucket = format!("{slug}-bucket");
    create_bucket(router, &bucket, REGION).await;
    let key_id = kms(router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");
    let role_arn = role(
        router,
        &format!("{slug}-role"),
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(&bucket)),
    )
    .await;
    Destination {
        bucket,
        role_arn,
        key_id,
    }
}

/// Take a DB snapshot of `instance` and wait for the snapshot to be the `available` one an
/// export may read, by polling the report rather than by waiting out a span.
async fn available_snapshot(router: &MultiServiceRouter, instance: &str, snapshot: &str) -> String {
    expect_ok(
        snapshot,
        call(
            router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    let request = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        snapshot,
        "the snapshot becoming available",
        || call(router, "DescribeDBSnapshots", &request),
        |status, body| status == 200 && text(body, "Status") == st::SNAPSHOT_STATUS_AVAILABLE,
    )
    .await;
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}")
}

/// Start the export of `source_arn` into `destination`, which writes its objects as it runs.
async fn export(
    router: &MultiServiceRouter,
    destination: &Destination,
    task: &str,
    source_arn: &str,
) {
    expect_ok(
        task,
        call(
            router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => task,
                "SourceArn" => source_arn,
                "S3BucketName" => destination.bucket,
                "IamRoleArn" => destination.role_arn,
                "KmsKeyId" => destination.key_id,
            ],
        )
        .await,
    );
}

/// The keys the destination bucket holds, through Amazon S3's own listing.
async fn listed_keys(router: &MultiServiceRouter, bucket: &str) -> Vec<String> {
    s3(router)
        .list_objects_v2()
        .bucket(bucket)
        .send()
        .await
        .expect("Amazon S3 lists the destination bucket")
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// The bytes Amazon S3 holds under one key.
async fn object_bytes(router: &MultiServiceRouter, bucket: &str, key: &str) -> Vec<u8> {
    s3(router)
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Amazon S3 holds the exported object {key}: {error}"))
        .body
        .collect()
        .await
        .expect("the exported object's bytes are read")
        .to_vec()
}

/// The tables a MySQL-family server holds in its own `mysql` schema on disk, which an export
/// of any source that server wrote therefore writes out.
///
/// Source: the MySQL Reference Manual's "Grant Tables" (the account catalog this server keeps
/// an account's identity and privileges in) and "Server-Side Help Support" (the four tables of
/// the help catalog). The rows below span both catalogs, not only the help topic table.
///
/// `pub(crate)` because every sibling suite that exports a MySQL source finds these objects
/// beside the client's own tables, and reads the list from the suite that owns the rule rather
/// than keeping a copy of it.
pub(crate) const MYSQL_SERVER_TABLES: [&str; 6] = [
    "help_topic",
    "help_category",
    "help_keyword",
    "help_relation",
    "user",
    "user_privileges",
];

/// The path each of those tables is written under by an export whose own prefix is
/// `export_prefix`: `<export prefix>mysql/mysql.<table>/`, the layout every exported table
/// takes.
pub(crate) fn server_table_prefixes(export_prefix: &str) -> Vec<String> {
    MYSQL_SERVER_TABLES
        .iter()
        .map(|table| format!("{export_prefix}mysql/mysql.{table}/"))
        .collect()
}

/// The schemas a MySQL server MATERIALISES rather than stores, which hold nothing for an
/// export to write.
const MATERIALISED_SCHEMAS: [&str; 2] = ["information_schema", "performance_schema"];

/// Rule: an export writes one Parquet object per table the source's storage holds, the
/// server's own stored tables included — so a snapshot of a DB instance no client has
/// connected to still exports the `mysql` schema, and nothing is written for the schemas the
/// server materialises.
#[tokio::test]
async fn an_export_writes_the_servers_own_stored_schema() {
    const INSTANCE: &str = "emu-server-schema-source";
    const SNAPSHOT: &str = "emu-server-schema-snapshot";
    const TASK: &str = "emu-server-schema-task";

    let (router, _engine) = emulator();
    // No session is ever opened on this instance: what it holds is what the SERVER came up
    // with.
    available_instance(&router, INSTANCE, "mysql", &[]).await;
    let destination = destination(&router, "emu-server-schema").await;
    let source_arn = available_snapshot(&router, INSTANCE, SNAPSHOT).await;
    export(&router, &destination, TASK, &source_arn).await;

    let keys = listed_keys(&router, &destination.bucket).await;
    let export_prefix = st::export_prefix(None, TASK);
    for table in MYSQL_SERVER_TABLES {
        let table_prefix = format!("{export_prefix}mysql/mysql.{table}/");
        assert!(
            keys.iter()
                .any(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet")),
            "case {table:?}: expected a Parquet object under {table_prefix}, got {keys:?}"
        );
    }
    for schema in MATERIALISED_SCHEMAS {
        let schema_prefix = format!("{export_prefix}{schema}/");
        assert!(
            !keys.iter().any(|key| key.starts_with(&schema_prefix)),
            "case {schema:?}: a materialised schema holds nothing to export, got {keys:?}"
        );
    }

    // And the file that says what was exported names every one of those tables.
    let manifest_key = keys
        .iter()
        .find(|key| key.contains("export_tables_info"))
        .unwrap_or_else(|| panic!("the export writes the file that says what it wrote, got {keys:?}"))
        .clone();
    let manifest = object_bytes(&router, &destination.bucket, &manifest_key).await;
    let document: Value = serde_json::from_slice(&manifest)
        .unwrap_or_else(|error| panic!("the export's JSON file must parse: {error}"));
    let entries = document
        .get("perTableStatus")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected perTableStatus in {document}"));
    for table in MYSQL_SERVER_TABLES {
        assert!(
            entries
                .iter()
                .any(|entry| entry.get("table") == Some(&Value::String(table.to_string()))),
            "case {table:?}: expected the exported table in the export's JSON file, got {document}"
        );
    }
}

/// One column of instants a session declares: how it is declared, the values written into it,
/// and what the export is expected to make of it.
struct InstantColumn {
    case: &'static str,
    /// The table the column is declared in, so each row is read back on its own.
    table: &'static str,
    /// The column's declared type, as a driver writes it.
    declared: &'static str,
    /// The two values written into it, in row order.
    values: [&'static str; 2],
    /// The microseconds since the epoch each value denotes, for a column exported as an
    /// instant; `None` for a row the rule does NOT make an instant column, which keeps the
    /// strings it holds.
    micros: Option<[i64; 2]>,
}

/// The rows: the two declared spellings of an instant on the MySQL engines, with and without
/// a fractional second, the ISO 8601 form with an offset, and the two boundaries of the rule —
/// a column declared an instant whose values denote none, and a column declared another
/// temporal type.
///
/// `2026-01-02 03:04:05` UTC is 1_767_323_045 seconds since the epoch, which is where every
/// expected value below comes from.
const INSTANT_COLUMNS: &[InstantColumn] = &[
    InstantColumn {
        case: "a DATETIME column holding the engine's own rendering",
        table: "made",
        declared: "DATETIME",
        values: ["2026-01-02 03:04:05", "2026-01-03 03:04:05"],
        micros: Some([1_767_323_045_000_000, 1_767_409_445_000_000]),
    },
    InstantColumn {
        case: "a DATETIME column with a fractional second",
        table: "made_fine",
        declared: "DATETIME(6)",
        values: ["2026-01-02 03:04:05.123456", "2026-01-02 03:04:05.5"],
        micros: Some([1_767_323_045_123_456, 1_767_323_045_500_000]),
    },
    InstantColumn {
        case: "a TIMESTAMP column",
        table: "stamped",
        declared: "TIMESTAMP",
        values: ["2026-01-02 03:04:05", "2026-01-02 03:04"],
        micros: Some([1_767_323_045_000_000, 1_767_323_040_000_000]),
    },
    InstantColumn {
        case: "a TIMESTAMP column holding the ISO 8601 form with an offset",
        table: "stamped_iso",
        declared: "TIMESTAMP",
        values: ["2026-01-02T03:04:05Z", "2026-01-02T04:04:05+01:00"],
        micros: Some([1_767_323_045_000_000, 1_767_323_045_000_000]),
    },
    InstantColumn {
        case: "a DATETIME column whose values denote no instant",
        table: "unparsed",
        declared: "DATETIME",
        values: ["0000-00-00 00:00:00", "whenever"],
        micros: None,
    },
    InstantColumn {
        case: "a DATE column, which denotes no instant",
        table: "dated",
        declared: "DATE",
        values: ["2026-01-02", "2026-01-03"],
        micros: None,
    },
];

/// Run `sql` in the session, failing the case when the engine refuses it.
async fn run(case: &str, session: &mut MySqlSession, sql: &str) {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"));
}

/// Rule: a column the source declared an instant is exported as a 64-bit Parquet timestamp in
/// microseconds carrying the instant its values denote; a column that denotes no instant keeps
/// the values it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instant_column_is_exported_as_a_parquet_timestamp() {
    const INSTANCE: &str = "emu-instant-export-source";
    const SNAPSHOT: &str = "emu-instant-export-snapshot";
    const TASK: &str = "emu-instant-export-task";
    const DATABASE: &str = "appdb";

    let (router, _engine) = emulator();
    let (address, port) = available_instance(&router, INSTANCE, "mysql", &[]).await;
    let mut session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("the master user must be admitted, got {refusal:?}"));
    for column in INSTANT_COLUMNS {
        run(
            column.case,
            &mut session,
            &format!(
                "CREATE TABLE {} (id INT PRIMARY KEY, created_at {})",
                column.table, column.declared
            ),
        )
        .await;
        for (index, value) in column.values.iter().enumerate() {
            run(
                column.case,
                &mut session,
                &format!(
                    "INSERT INTO {} VALUES ({}, '{value}')",
                    column.table,
                    index + 1
                ),
            )
            .await;
        }
    }

    let destination = destination(&router, "emu-instant-export").await;
    let source_arn = available_snapshot(&router, INSTANCE, SNAPSHOT).await;
    export(&router, &destination, TASK, &source_arn).await;
    let keys = listed_keys(&router, &destination.bucket).await;
    let export_prefix = st::export_prefix(None, TASK);

    for column in INSTANT_COLUMNS {
        let table_prefix = format!("{export_prefix}{DATABASE}/{DATABASE}.{}/", column.table);
        let key = keys
            .iter()
            .find(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet"))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a Parquet object under {table_prefix}, got {keys:?}",
                    column.case
                )
            })
            .clone();
        let parquet = object_bytes(&router, &destination.bucket, &key).await;
        let reader = SerializedFileReader::new(Bytes::from(parquet)).unwrap_or_else(|error| {
            panic!("case {:?}: the object is Parquet: {error}", column.case)
        });
        let schema = reader.metadata().file_metadata().schema_descr();
        let exported = (0..schema.num_columns())
            .find(|index| schema.column(*index).name() == "created_at")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the exported file carries the source's own column",
                    column.case
                )
            });
        let physical = schema.column(exported).physical_type();
        let converted = schema.column(exported).converted_type();
        let rows: Vec<::parquet::record::Row> = reader
            .get_row_iter(None)
            .unwrap_or_else(|error| {
                panic!("case {:?}: the Parquet rows read: {error}", column.case)
            })
            .filter_map(Result::ok)
            .collect();
        match column.micros {
            // An instant column: a 64-bit leaf read as microseconds, carrying every value.
            Some(expected) => {
                assert_eq!(
                    (physical, converted),
                    (PhysicalType::INT64, ConvertedType::TIMESTAMP_MICROS),
                    "case {:?}: expected a 64-bit Parquet timestamp in microseconds, got \
                     {physical} with converted type {converted}",
                    column.case
                );
                let written: Vec<i128> = rows
                    .iter()
                    .filter_map(|row| row.get_timestamp_micros(exported).ok())
                    .map(i128::from)
                    .collect();
                let wanted: Vec<i128> = expected.iter().map(|value| i128::from(*value)).collect();
                assert_eq!(
                    written, wanted,
                    "case {:?}: expected the instants the source holds, got {written:?}",
                    column.case
                );
            }
            // Not an instant column: the values it holds, carried as the strings they are.
            None => {
                assert_eq!(
                    (physical, converted),
                    (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
                    "case {:?}: expected the values the column holds, got {physical} with \
                     converted type {converted}",
                    column.case
                );
                let written: Vec<String> = rows
                    .iter()
                    .filter_map(|row| row.get_string(exported).ok().cloned())
                    .collect();
                assert_eq!(
                    written,
                    column.values.to_vec(),
                    "case {:?}: expected the values the source holds, got {written:?}",
                    column.case
                );
            }
        }
    }
}
