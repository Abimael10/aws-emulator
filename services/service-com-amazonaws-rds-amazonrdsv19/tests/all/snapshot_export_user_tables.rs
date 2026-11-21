// SPDX-License-Identifier: CC0-1.0

//! What a snapshot export of a DB INSTANCE writes, and how a client polling it reaches the
//! end of the export.
//!
//! Two rules are pinned here, over the one flow a client drives — create a DB instance, fill
//! it in over the endpoint it publishes, snapshot it, start the export and poll the task:
//!
//! 1. **The flow CONVERGES.** Every state the next step waits for arrives on this service's
//!    own clock, with no mutating call in between: the instance reports `available` with the
//!    endpoint it published, the snapshot reports `available`, and the export task walks its
//!    progression to the terminal `COMPLETE` carrying `PercentProgress` 100, a `TaskEndTime`
//!    and no `FailureCause`, with its data objects and its own JSON files in the destination
//!    bucket under `<S3Prefix>/<ExportTaskIdentifier>/`. A client that polls
//!    `DescribeExportTasks`, which is what the CLI's and the SDKs' waiters do, therefore ends
//!    its loop.
//! 2. **An export writes the SOURCE'S OWN data and nothing else.** The relations the ENGINE
//!    owns are not the customer's data and are not exported: on the MySQL engines its catalog
//!    lives in system DATABASES of its own, and on the PostgreSQL engines it lives beside the
//!    customer's tables INSIDE every database, under the `pg_` prefix PostgreSQL reserves for
//!    it — so an export writes a Parquet object for each of the customer's tables and none
//!    for `pg_class`, `pg_settings` or any other catalog relation, whether or not a session
//!    materialised them by reading them. `ExportOnly` then narrows that set further.
//!
//! The PostgreSQL rows span what the second rule generalises over: the whole snapshot written
//! at the root of the bucket, the whole snapshot under a prefix, a database named by itself,
//! and one schema-qualified table — the forms `ExportOnly` publishes for such a source. Each
//! row's source is seeded over the engine's own wire with a stock client's statements, and
//! each row READS a catalog relation before it is snapshotted, so the catalog really is in
//! the storage the export walks. The second test drives the other engine family the export
//! supports, whose catalog is laid out the other way, over the same flow.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, and the objects are listed through Amazon S3's own
//! `ListObjectsV2`, so what this asserts exists is what that service really holds.

use std::time::{Duration, Instant};

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, master_session, must_run, mysql_login, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, expect_ok, optional_text, text, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role, s3};

/// How long a poll loop here is given before it calls a state unreachable: generous next to
/// the span this service owes a transition in, so a loaded machine cannot fail the suite,
/// and nothing in it is timed.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// The gap between two reads of the same loop.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The PostgreSQL schema a database's own tables live in, which is the schema segment of
/// their exported objects. On the MySQL engines, which have no schema layer, that segment is
/// the database's own name.
const POSTGRES_SCHEMA: &str = "public";

/// The suffix every data object of an export carries: one GZIP-compressed Parquet part file
/// per table.
const PARQUET_SUFFIX: &str = ".gz.parquet";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One export this suite drives.
struct Row {
    case: &'static str,
    /// The identifiers of the row's own resources, which also name its export task.
    slug: &'static str,
    /// `S3Prefix`, when the row names one.
    prefix: Option<&'static str>,
    /// `ExportOnly`, empty where the whole source is exported.
    export_only: &'static [&'static str],
    /// The tables of [`DATABASE`] the export must write, and only these.
    expected_tables: &'static [&'static str],
}

/// The statements every PostgreSQL row's session runs in the database the instance came
/// with, declaring its tables with the types a stock client declares them with.
const POSTGRES_SEED: &[&str] = &[
    "CREATE TABLE orders (id INT PRIMARY KEY, total NUMERIC(10,2), placed_at TIMESTAMP)",
    "INSERT INTO orders VALUES (1, 10.25, '2026-01-01 00:00:00')",
    "INSERT INTO orders VALUES (2, 3.50, '2026-01-02 00:00:00')",
    "CREATE TABLE regions (code VARCHAR(8) PRIMARY KEY, name TEXT)",
    "INSERT INTO regions VALUES ('wst', 'west')",
    // And a READ of the server's own catalog, which is what materialises those relations in
    // this database: the export must leave them out even so.
    "SELECT name FROM pg_settings",
    "SELECT relname FROM pg_class",
];

/// The tables the seed above leaves in the database, which is what an unrestricted export
/// writes.
const POSTGRES_SEEDED_TABLES: &[&str] = &["orders", "regions"];

const POSTGRES_ROWS: &[Row] = &[
    Row {
        case: "the whole snapshot, at the root of the bucket",
        slug: "pgx-all",
        prefix: None,
        export_only: &[],
        expected_tables: POSTGRES_SEEDED_TABLES,
    },
    Row {
        case: "the whole snapshot, under a prefix",
        slug: "pgx-prefixed",
        prefix: Some("nightly/exports"),
        export_only: &[],
        expected_tables: POSTGRES_SEEDED_TABLES,
    },
    Row {
        case: "a database named by itself",
        slug: "pgx-database",
        prefix: Some("archive"),
        export_only: &["appdb"],
        expected_tables: POSTGRES_SEEDED_TABLES,
    },
    Row {
        case: "one schema-qualified table",
        slug: "pgx-one-table",
        prefix: Some("partial"),
        export_only: &["appdb.public.orders"],
        expected_tables: &["orders"],
    },
];

/// Read `look` until it answers `Ok`, or [`POLL_DEADLINE`] passes.
///
/// The shape of the loop a client writes, and half of what this suite is about: `look` makes
/// a REPORTING call and nothing else, so only a transition this service's clock owes can end
/// it.
async fn poll_until<Look, Fut, T>(case: &str, what: &str, look: Look) -> T
where
    Look: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let last = match look().await {
            Ok(settled) => return settled,
            Err(seen) => seen,
        };
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {what} never settled; last read {last}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Snapshot `instance` and poll the snapshot to `available`, answering its ARN — the source
/// an export of it names.
async fn available_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    instance: &str,
    snapshot_id: &str,
) -> String {
    expect_ok(
        case,
        call(
            router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot_id,
            ],
        )
        .await,
    );
    let described = poll_until(case, "the snapshot reaches `available`", || async {
        let described = expect_ok(
            case,
            call(
                router,
                "DescribeDBSnapshots",
                &params!["DBSnapshotIdentifier" => snapshot_id],
            )
            .await,
        );
        match text(&described, "Status") == st::SNAPSHOT_STATUS_AVAILABLE {
            true => Ok(described),
            false => Err(described),
        }
    })
    .await;
    text(&described, "DBSnapshotArn")
}

/// Export `snapshot_arn` into a bucket of this row's own, poll the task to its terminal
/// status, and answer the `(key, size)` of every object the export left under its prefix.
///
/// The terminal shape a polling client branches on is asserted here, once, for every row:
/// `COMPLETE` with all of the source exported, an end time and no failure cause.
async fn exported_objects(
    router: &MultiServiceRouter,
    case: &str,
    slug: &str,
    snapshot_arn: &str,
    prefix: Option<&str>,
    export_only: &[&str],
) -> Vec<(String, i64)> {
    let bucket = format!("{slug}-destination");
    create_bucket(router, &bucket, REGION).await;
    let key_id = kms(router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: KMS creates the export key: {error}"))
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the created key reports its id"));
    let role_arn = role(
        router,
        &format!("{slug}-role"),
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(&bucket)),
    )
    .await;

    let mut request: Vec<(&str, String)> = params![
        "ExportTaskIdentifier" => slug,
        "SourceArn" => snapshot_arn,
        "S3BucketName" => bucket,
        "IamRoleArn" => role_arn,
        "KmsKeyId" => key_id,
    ]
    .to_vec();
    if let Some(prefix) = prefix {
        request.push(("S3Prefix", prefix.to_string()));
    }
    for (index, entry) in export_only.iter().enumerate() {
        // The awsQuery form of one entry of a list member, indexed from one.
        request.push((
            Box::leak(format!("ExportOnly.member.{}", index + 1).into_boxed_str()),
            (*entry).to_string(),
        ));
    }
    let started = expect_ok(case, call(router, "StartExportTask", &request).await);
    assert_eq!(
        text(&started, "Status"),
        st::EXPORT_TASK_STATUS_STARTING,
        "case {case:?}: an export just accepted is STARTING, got {started}"
    );

    // The terminal status a client polls for, reached on the clock alone.
    let settled = poll_until(case, "the export task reaches a terminal status", || async {
        let described = expect_ok(
            case,
            call(
                router,
                "DescribeExportTasks",
                &params!["ExportTaskIdentifier" => slug],
            )
            .await,
        );
        match st::TERMINAL_STATUSES.contains(&text(&described, "Status").as_str()) {
            true => Ok(described),
            false => Err(described),
        }
    })
    .await;
    assert_eq!(
        text(&settled, "Status"),
        st::EXPORT_TASK_STATUS_COMPLETE,
        "case {case:?}: an export that wrote its objects ends COMPLETE, got {settled}"
    );
    assert_eq!(
        text(&settled, "PercentProgress"),
        st::COMPLETE_PERCENT_PROGRESS.to_string(),
        "case {case:?}: a completed export exported all of its source, got {settled}"
    );
    assert_eq!(
        optional_text(&settled, "FailureCause"),
        None,
        "case {case:?}: a completed export carries no failure cause, got {settled}"
    );
    assert!(
        optional_text(&settled, "TaskEndTime").is_some(),
        "case {case:?}: a task that ended reports when it ended, got {settled}"
    );

    s3(router)
        .list_objects_v2()
        .bucket(&bucket)
        .prefix(st::export_prefix(prefix, slug))
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: Amazon S3 lists the destination: {error}"))
        .contents()
        .iter()
        .filter_map(|object| Some((object.key()?.to_string(), object.size().unwrap_or(0))))
        .collect()
}

/// Assert `objects` holds one non-empty Parquet object per expected table and no other data
/// object, and that the export's own two JSON files are beside them.
fn wrote_exactly(
    case: &str,
    slug: &str,
    prefix: Option<&str>,
    objects: &[(String, i64)],
    table_prefixes: &[String],
) {
    let keys: Vec<&String> = objects.iter().map(|(key, _)| key).collect();
    for table_prefix in table_prefixes {
        let written: Vec<&(String, i64)> = objects
            .iter()
            .filter(|(key, _)| key.starts_with(table_prefix) && key.ends_with(PARQUET_SUFFIX))
            .collect();
        assert_eq!(
            written.len(),
            1,
            "case {case:?}: expected one Parquet object under {table_prefix}, got {keys:?}"
        );
        assert!(
            written[0].1 > 0,
            "case {case:?}: the Parquet object {} must carry the table's rows, got {} bytes",
            written[0].0,
            written[0].1
        );
    }
    // And NO data object for anything else: not for a table the export was not asked for,
    // and not for a relation of the server's own catalog.
    let data_objects: Vec<&String> = keys
        .iter()
        .filter(|key| key.ends_with(PARQUET_SUFFIX))
        .copied()
        .collect();
    assert_eq!(
        data_objects.len(),
        table_prefixes.len(),
        "case {case:?}: an export writes the source's own tables and nothing else \
         (expected {table_prefixes:?}), got {data_objects:?}"
    );
    // The two files the export writes beside its data, naming what it covered.
    let export_prefix = st::export_prefix(prefix, slug);
    for named in [
        st::tables_info_key(&export_prefix, slug, table_prefixes.len()),
        st::export_info_key(&export_prefix, slug),
    ] {
        assert!(
            keys.iter().any(|key| **key == named),
            "case {case:?}: expected the export's own file at {named}, got {keys:?}"
        );
    }
}

/// Rule: the export of a wire-seeded PostgreSQL snapshot settles COMPLETE and writes the
/// customer's tables — never the engine's own catalog relations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_postgres_snapshot_export_writes_the_user_tables_and_settles_complete() {
    let (router, _engine) = emulator();
    for row in POSTGRES_ROWS {
        let Row {
            case,
            slug,
            prefix,
            export_only,
            expected_tables,
        } = *row;
        let instance = format!("{slug}-source");

        // ---- the source: an instance, seeded over the endpoint it publishes ----
        let (address, port) = available_instance(&router, &instance, "postgres", &[]).await;
        let mut session = master_session(case, &address, port).await;
        for statement in POSTGRES_SEED {
            must_run(case, &mut session, statement).await;
        }

        let snapshot_arn =
            available_snapshot(&router, case, &instance, &format!("{slug}-snapshot")).await;
        let objects =
            exported_objects(&router, case, slug, &snapshot_arn, prefix, export_only).await;
        let export_prefix = st::export_prefix(prefix, slug);
        let table_prefixes: Vec<String> = expected_tables
            .iter()
            .map(|table| format!("{export_prefix}{DATABASE}/{POSTGRES_SCHEMA}.{table}/"))
            .collect();
        wrote_exactly(case, slug, prefix, &objects, &table_prefixes);
    }
}

/// The same rules on the other engine family the export supports, whose catalog lives in
/// system DATABASES of the server's own rather than inside the customer's: the export writes
/// the table the session created and nothing out of `mysql`, `information_schema`,
/// `performance_schema` or `sys`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mysql_snapshot_export_writes_the_user_tables_and_settles_complete() {
    const CASE: &str = "a wire-seeded MySQL instance";
    const SLUG: &str = "myx-all";
    const PREFIX: Option<&str> = Some("exports");

    let (router, _engine) = emulator();
    let instance = format!("{SLUG}-source");
    let (address, port) = available_instance(&router, &instance, "mysql", &[]).await;
    // The session a stock driver opens: it turns autocommit off, which is what a driver's
    // default transaction handling does, and commits what it wrote.
    let mut session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {CASE:?}: the master user must be admitted: {refusal:?}")
    });
    for statement in [
        "SET AUTOCOMMIT = 0",
        "CREATE TABLE orders (id INT PRIMARY KEY, total DECIMAL(10,2))",
        "INSERT INTO orders VALUES (1, 10.25)",
        "COMMIT",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {CASE:?}: {statement:?} must run: {refusal:?}"));
    }

    let snapshot_arn =
        available_snapshot(&router, CASE, &instance, &format!("{SLUG}-snapshot")).await;
    let objects = exported_objects(&router, CASE, SLUG, &snapshot_arn, PREFIX, &[]).await;
    let export_prefix = st::export_prefix(PREFIX, SLUG);
    // On a MySQL source the schema segment is the database's own name, since the engine has
    // no schema layer of its own.
    let table_prefixes = vec![format!("{export_prefix}{DATABASE}/{DATABASE}.orders/")];
    wrote_exactly(CASE, SLUG, PREFIX, &objects, &table_prefixes);
}
