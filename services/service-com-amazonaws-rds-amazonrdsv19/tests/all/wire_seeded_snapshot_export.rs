// SPDX-License-Identifier: CC0-1.0

//! What a snapshot EXPORT writes for a database a client filled in over the engine's own
//! wire.
//!
//! The rule this suite pins: **an export writes one Apache Parquet object per table the
//! source's storage holds, whoever created it** — a table a client created in a session on the
//! published endpoint is part of the instance's storage, so the snapshot taken of that
//! instance carries it and the export of that snapshot writes it. The suite drives the MySQL
//! protocol client the sibling endpoint suites drive, so what is exercised is what a stock
//! driver sends: a session that turns autocommit off, creates its own database, declares a
//! table and commits, which is how a driver's default transaction handling writes rows.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, and the objects the export wrote are listed through Amazon
//! S3's own `ListObjectsV2` — so what this asserts exists is what that service really holds.
//!
//! The rows span the two places a client's table can live: the database the instance was
//! created with (`DBName`) and one the session created itself, and the column types a driver
//! declares them with.

use crate::db_instance_engine_endpoint::{available_instance, mysql_login, MySqlSession, Transport};
use crate::db_instances::{call, expect_ok, wait_for_settle, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role, s3};
use service_com_amazonaws_rds_amazonrdsv19::state as st;

/// The master user the instance below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The database the instance comes with, which `DBName` named.
const DATABASE: &str = "appdb";

/// The canonical account id every generated emulator presents, for the ARN the export names
/// its source by.
const ACCOUNT: &str = "000000000000";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One table a session writes: the database it lives in, and the statements that create it.
struct SeededTable {
    case: &'static str,
    database: &'static str,
    /// The statements the session runs for it, in order — the database it needs included.
    statements: &'static [&'static str],
    table: &'static str,
}

/// The tables the session writes: one in the database the instance came with and one in a
/// database the session creates, declared with the types a driver writes (`INT`
/// `AUTO_INCREMENT`, `VARCHAR`, `DECIMAL`, `DATETIME`) rather than the engine's own. Each
/// row's statements name the database they run against, because one of the two rows creates
/// its own.
const SEEDED: &[SeededTable] = &[
    SeededTable {
        case: "the database the instance came with",
        database: DATABASE,
        statements: &[
            "USE appdb",
            "CREATE TABLE widgets (id INT PRIMARY KEY, name VARCHAR(50), price DECIMAL(10,2), \
             made_at DATETIME)",
            "INSERT INTO widgets VALUES (1, 'alpha', 10.25, '2026-01-01 00:00:00')",
            "INSERT INTO widgets VALUES (2, 'beta', 3.50, '2026-01-02 00:00:00')",
        ],
        table: "widgets",
    },
    SeededTable {
        case: "a database the session created",
        database: "ledger",
        statements: &[
            "CREATE DATABASE IF NOT EXISTS ledger",
            "USE ledger",
            "CREATE TABLE marks (id INT AUTO_INCREMENT PRIMARY KEY, score DECIMAL(10,2)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            "INSERT INTO marks (score) VALUES (1.50), (2.25)",
        ],
        table: "marks",
    },
];

/// Run `sql`, failing the case when the engine refuses it.
async fn run(session: &mut MySqlSession, sql: &str) {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("{sql:?} must run, got {refusal:?}"));
}

/// Rule: an export writes a Parquet object for every table the source holds, including the
/// tables a CLIENT created over the engine's own wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_export_writes_the_tables_a_client_created_over_the_wire() {
    const INSTANCE: &str = "emu-wire-export-source";
    const BUCKET: &str = "emu-wire-export-bucket";
    const TASK: &str = "emu-wire-export-task";
    const SNAPSHOT: &str = "emu-wire-export-snapshot";

    let (router, _engine) = emulator();
    let (address, port) = available_instance(&router, INSTANCE, "mysql", &[]).await;

    // The session a stock driver opens: it names no database, turns autocommit off — which is
    // what a driver's default transaction handling does — and commits what it wrote.
    let mut session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        None,
    )
    .await
    .unwrap_or_else(|refusal| panic!("the master user must be admitted, got {refusal:?}"));
    run(&mut session, "SET AUTOCOMMIT = 0").await;
    for seeded in SEEDED {
        for statement in seeded.statements {
            run(&mut session, statement).await;
        }
        run(&mut session, "COMMIT").await;
    }

    // The snapshot taken of that instance, and everything the export of it needs.
    expect_ok(
        "snapshot the source",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => INSTANCE,
                "DBSnapshotIdentifier" => SNAPSHOT,
            ],
        )
        .await,
    );
    wait_for_settle();
    create_bucket(&router, BUCKET, REGION).await;
    let key_id = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");
    let role_arn = role(
        &router,
        "emu-wire-export-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(BUCKET)),
    )
    .await;
    expect_ok(
        "export the snapshot",
        call(
            &router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => TASK,
                "SourceArn" => format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{SNAPSHOT}"),
                "S3BucketName" => BUCKET,
                "IamRoleArn" => role_arn,
                "KmsKeyId" => key_id,
            ],
        )
        .await,
    );

    // The objects the export wrote, read back through Amazon S3's own listing.
    let keys: Vec<String> = s3(&router)
        .list_objects_v2()
        .bucket(BUCKET)
        .send()
        .await
        .expect("Amazon S3 lists the destination bucket")
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect();
    for seeded in SEEDED {
        let table_prefix = format!(
            "{}{}/{}.{}/",
            st::export_prefix(None, TASK),
            seeded.database,
            seeded.database,
            seeded.table
        );
        assert!(
            keys.iter()
                .any(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet")),
            "case {:?}: expected a Parquet object under {table_prefix}, got {keys:?}",
            seeded.case
        );
    }
}
