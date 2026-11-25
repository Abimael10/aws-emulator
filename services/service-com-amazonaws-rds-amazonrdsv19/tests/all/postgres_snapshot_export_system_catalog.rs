// SPDX-License-Identifier: CC0-1.0

//! What a snapshot EXPORT reports for a database that held nothing to write, and what a
//! PostgreSQL server's own storage carries before any client has connected to it.
//!
//! The rules this suite pins:
//!
//! 1. **A PostgreSQL server holds the database its own initialisation creates (`postgres`)
//!    from the moment its endpoint answers**, exactly as a MySQL-family server holds its own
//!    schema ([`crate::snapshot_export_server_schema`]). So a snapshot taken of an instance no
//!    client has ever connected to carries that database, and an export of that snapshot
//!    COVERS it. What it does NOT carry is the PostgreSQL system catalog: a PostgreSQL server
//!    rebuilds those relations from its own state when a session reads them, so they are not
//!    part of the storage a snapshot carries and no export writes one.
//! 2. **The export's own JSON file reports one `perTableStatus` entry per target it covered**:
//!    a table it wrote is `COMPLETE`, and a database it covered that held no table of its own
//!    is `SKIPPED` with the reason nothing was written for it, `DATABASE_IS_EMPTY`. So a
//!    client that reads that file to discover what the export covered is told about an empty
//!    database rather than left to infer its absence, and a source with no table at all still
//!    reports what it covered.
//! 2. **The export's own JSON file reports one `perTableStatus` entry per target it covered**:
//!    a table it wrote is `COMPLETE`, and a database it covered that held no table of its own
//!    is `SKIPPED` with the reason nothing was written for it, `DATABASE_IS_EMPTY`. So a
//!    client that reads that file to discover what the export covered is told about an empty
//!    database rather than left to infer its absence, and a source with no table at all still
//!    reports what it covered.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, and every object asserted here is read back through Amazon
//! S3's own `ListObjectsV2` and `GetObject` — so what this suite finds is what that service
//! really holds.
//!
//! The rows span what the second rule generalises over: a source whose every database is empty
//! (no client ever connected), a source whose databases are empty beside one a client filled
//! in, a source whose tables all live in one database a client created itself, and the other
//! engine family the export supports, whose server's own schema is never empty. Each row's
//! own `ExportOnly` spans the forms that decide which databases the export covers at all.

use serde_json::Value;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, mysql_login, postgres_login, PostgresSession, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role, s3};

/// The canonical account id every generated emulator presents, for the ARN an export names
/// its source by.
const ACCOUNT: &str = "000000000000";

/// The database a PostgreSQL instance always has beside the one `DBName` named: `postgres`,
/// which the server's own initialisation creates.
const SERVER_DATABASE: &str = "postgres";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One row of the table: the source, what a client wrote into it, and what the export's own
/// file must then report.
struct Row {
    case: &'static str,
    slug: &'static str,
    engine: &'static str,
    /// The statements a client runs over the engine's own wire, `&[]` for a source no client
    /// has connected to. Each runs against the database the instance came with.
    seeded: &'static [&'static str],
    /// `ExportOnly`, empty where the whole source is exported.
    export_only: &'static [&'static str],
    /// The `<database>.<table>` targets the export must report as written.
    written: &'static [&'static str],
    /// The databases it must report as skipped for being empty.
    empty: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a PostgreSQL source no client has connected to",
        slug: "emu-empty-bare",
        engine: "postgres",
        seeded: &[],
        export_only: &[],
        written: &[],
        // The database the server came up with holds nothing: no client has written to this
        // source at all. The database `DBName` named is created as the first session is
        // admitted, and no session was.
        empty: &[SERVER_DATABASE],
    },
    Row {
        case: "a PostgreSQL source a client filled one database in",
        slug: "emu-empty-seeded",
        engine: "postgres",
        seeded: &[
            "CREATE TABLE widgets (id integer PRIMARY KEY, name text)",
            "INSERT INTO widgets VALUES (1, 'alpha')",
        ],
        export_only: &[],
        written: &["appdb.widgets"],
        empty: &[SERVER_DATABASE],
    },
    Row {
        case: "a PostgreSQL source exported for the database a client filled in alone",
        slug: "emu-empty-narrowed",
        engine: "postgres",
        seeded: &[
            "CREATE TABLE widgets (id integer PRIMARY KEY, name text)",
            "INSERT INTO widgets VALUES (1, 'alpha')",
        ],
        // `ExportOnly` naming that database covers it and nothing else, so the server's own
        // empty database is not a target of this export at all.
        export_only: &[DATABASE],
        written: &["appdb.widgets"],
        empty: &[],
    },
    Row {
        case: "a MySQL source, whose server's own schema is never empty",
        slug: "emu-empty-mysql",
        engine: "mysql",
        seeded: &["CREATE TABLE widgets (id INT PRIMARY KEY, name VARCHAR(20))"],
        export_only: &[DATABASE],
        written: &["appdb.widgets"],
        empty: &[],
    },
];

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

/// A PostgreSQL session on the instance's own endpoint as its master user, against the
/// database it came with — opened over TLS, which an RDS for PostgreSQL instance's own
/// parameter group requires.
async fn postgres_session(address: &str, port: u16, case: &str) -> PostgresSession {
    postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted, got {refusal:?}")
    })
}

/// Run every statement of `row` over its engine's own wire, in one session on the database the
/// instance came with.
async fn seed(row: &Row, address: &str, port: u16) {
    if row.seeded.is_empty() {
        return;
    }
    match row.engine {
        "postgres" => {
            let mut session = postgres_session(address, port, row.case).await;
            for statement in row.seeded {
                session.query(statement).await.unwrap_or_else(|refusal| {
                    panic!(
                        "case {:?}: {statement:?} must run, got {refusal:?}",
                        row.case
                    )
                });
            }
        }
        _ => {
            let mut session = mysql_login(
                address,
                port,
                Transport::Plaintext,
                MASTER_USER,
                MASTER_PASSWORD,
                Some(DATABASE),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {:?}: the master user must be admitted, got {refusal:?}",
                    row.case
                )
            });
            for statement in row.seeded {
                session.query(statement).await.unwrap_or_else(|refusal| {
                    panic!(
                        "case {:?}: {statement:?} must run, got {refusal:?}",
                        row.case
                    )
                });
            }
        }
    }
}

/// Rule: the export's own file reports one entry per target it covered — `COMPLETE` for a
/// table it wrote, `SKIPPED` with `DATABASE_IS_EMPTY` for a database it covered that held no
/// table — so a source that held nothing to write still says what it covered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_reports_every_database_it_covered_including_the_empty_ones() {
    for row in ROWS {
        let instance = format!("{}-source", row.slug);
        let snapshot = format!("{}-snapshot", row.slug);
        let task = format!("{}-task", row.slug);
        let (router, _engine) = emulator();
        let (address, port) = available_instance(&router, &instance, row.engine, &[]).await;
        seed(row, &address, port).await;

        let destination = destination(&router, row.slug).await;
        let source_arn = available_snapshot(&router, &instance, &snapshot).await;
        let mut request: Vec<(&str, String)> = params![
            "ExportTaskIdentifier" => task,
            "SourceArn" => source_arn,
            "S3BucketName" => destination.bucket,
            "IamRoleArn" => destination.role_arn,
            "KmsKeyId" => destination.key_id,
        ]
        .to_vec();
        for (index, entry) in row.export_only.iter().enumerate() {
            // The awsQuery form of one entry of a list member, indexed from one.
            request.push((
                Box::leak(format!("ExportOnly.member.{}", index + 1).into_boxed_str()),
                (*entry).to_string(),
            ));
        }
        expect_ok(row.case, call(&router, "StartExportTask", &request).await);

        let keys = listed_keys(&router, &destination.bucket).await;
        let export_prefix = st::export_prefix(None, &task);
        let manifest_key = keys
            .iter()
            .find(|key| key.contains("export_tables_info"))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the export writes the file that says what it wrote, got {keys:?}",
                    row.case
                )
            })
            .clone();
        let manifest = object_bytes(&router, &destination.bucket, &manifest_key).await;
        let document: Value = serde_json::from_slice(&manifest).unwrap_or_else(|error| {
            panic!(
                "case {:?}: the export's JSON file must parse: {error}",
                row.case
            )
        });
        let entries = document
            .get("perTableStatus")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {:?}: expected perTableStatus in {document}", row.case)
            });
        assert!(
            !entries.is_empty(),
            "case {:?}: an export reports every target it covered, got {document}",
            row.case
        );

        // The tables it wrote: one COMPLETE entry each, every one of them beside its own
        // Parquet object.
        for target in row.written {
            let (database, table) = target
                .split_once('.')
                .unwrap_or_else(|| panic!("case {:?}: {target} names a table", row.case));
            let entry = entries
                .iter()
                .find(|entry| {
                    entry.get("database").and_then(Value::as_str) == Some(database)
                        && entry.get("table").and_then(Value::as_str) == Some(table)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected {target} in the export's JSON file, got {document}",
                        row.case
                    )
                });
            assert_eq!(
                entry.get("status").and_then(Value::as_str),
                Some(st::EXPORT_TASK_STATUS_COMPLETE),
                "case {:?}: a table that was written is COMPLETE, got {entry}",
                row.case
            );
            assert!(
                keys.iter()
                    .any(|key| key.starts_with(&export_prefix) && key.ends_with(".gz.parquet")),
                "case {:?}: expected the Parquet object of {target}, got {keys:?}",
                row.case
            );
        }

        // The databases it covered that held nothing: one SKIPPED entry each, carrying the
        // reason nothing was written for them — and no entry for a database the export was
        // not asked for.
        let skipped: Vec<(String, String)> = entries
            .iter()
            .filter(|entry| {
                entry.get("status").and_then(Value::as_str) == Some(st::EXPORT_TARGET_STATUS_SKIPPED)
            })
            .map(|entry| {
                (
                    entry
                        .get("target")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    entry
                        .get("skipReason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        let wanted: Vec<(String, String)> = row
            .empty
            .iter()
            .map(|database| {
                (
                    (*database).to_string(),
                    st::SKIP_REASON_DATABASE_IS_EMPTY.to_string(),
                )
            })
            .collect();
        assert_eq!(
            skipped, wanted,
            "case {:?}: expected the skipped databases {wanted:?}, got {document}",
            row.case
        );
        // And the count it reports is the tables it WROTE, which a skipped database is not
        // one of.
        assert_eq!(
            document.get("tablesExported").and_then(Value::as_u64),
            Some(
                entries
                    .iter()
                    .filter(|entry| {
                        entry.get("status").and_then(Value::as_str)
                            == Some(st::EXPORT_TASK_STATUS_COMPLETE)
                    })
                    .count() as u64
            ),
            "case {:?}: tablesExported counts the tables that were written, got {document}",
            row.case
        );
    }
}
