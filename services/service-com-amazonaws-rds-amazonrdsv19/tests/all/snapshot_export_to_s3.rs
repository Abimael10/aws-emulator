// SPDX-License-Identifier: CC0-1.0

//! What a snapshot or cluster EXPORT to Amazon S3 leaves behind, and what it refuses.
//!
//! The rules this suite pins, each over a table spanning what it generalises over:
//!
//! * An export writes the SOURCE's own data: one Apache Parquet object per table of the
//!   database the source carries, under `<S3Prefix>/<ExportTaskIdentifier>/<database>/
//!   <schema>.<table>/`. Every source kind `SourceArn` may name carries that data — a DB
//!   snapshot and a DB cluster snapshot each hold the copy the engine took when the
//!   snapshot was taken, and a DB cluster holds its live cluster volume — so the kind of
//!   source decides nothing about whether the data is written.
//! * A completed export also writes its own SUMMARY object,
//!   `export_info_<ExportTaskIdentifier>.json`, describing the export as a whole: what it
//!   read, where it wrote, the status it ended in and the per-table row counts.
//! * A column the source declared an EXACT numeric type is exported as a Parquet DECIMAL
//!   carrying that column's precision and scale, so an exact value stays exact; the
//!   approximate and integer types keep their own Parquet types.
//! * Each refusal of an export carries the status, the code and the SENTENCE Amazon RDS
//!   words that error class in, with the request's own values in its variable parts.
//! * Whether an export is ACCEPTED is decided by whether the destination bucket and the
//!   encryption key the request names resolve: a request refused for either records no task
//!   and writes no object, and the same request with that one parameter replaced by a
//!   resolvable one is accepted and walks to the terminal `COMPLETE` with the export's
//!   objects under its own prefix.
//! * Every condition that leaves the key named by `KmsKeyId` unable to encrypt the export —
//!   a key KMS holds nothing under, a key it holds disabled, and a key whose own policy
//!   withholds either of the two KMS operations the export performs on it — is answered with
//!   the one `KMSKeyNotAccessibleFault` and the single sentence that fault carries here,
//!   naming the key reference the request passed and nothing else.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them, and every object asserted here is read back through Amazon
//! S3's own `ListObjectsV2` and `GetObject`, so what this suite finds is what that service
//! really holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parquet::basic::{ConvertedType, Type as PhysicalType};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use serde_json::Value;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;
use tokio::time::sleep;

use crate::db_instances::{call, expect_error, expect_ok, subtrees, text, texts, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, export_role_policy, kms, role, s3};
use crate::snapshot_export_server_schema::MYSQL_SERVER_TABLES;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A second Region, for the row that names a bucket outside the export's own.
const OTHER_REGION: &str = "eu-west-1";

/// The master user every source below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The database the exported tables live in.
const DATABASE: &str = "appdb";

/// How long a poll loop here is given before it calls a transition unobservable: generous
/// next to the span this service owes one after, so a loaded machine cannot fail the suite.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// The gap between two polls of one loop.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Poll `read` until `settled` holds, answering the body it settled on; panics with the last
/// answer when [`POLL_DEADLINE`] passes first. The loop waits for the CONDITION, so a faster
/// or slower machine only changes how many times it looks.
async fn poll_until<Read, Fut>(case: &str, what: &str, read: Read, settled: impl Fn(&str) -> bool) -> String
where
    Read: Fn() -> Fut,
    Fut: std::future::Future<Output = (u16, String)>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let (status, body) = read().await;
        if status == 200 && settled(&body) {
            return body;
        }
        last = format!("HTTP {status}: {body}");
        sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {what} never settled in {POLL_DEADLINE:?}; last answer: {last}")
}

/// One table a source is seeded with: its name, the statement that creates it and the rows
/// inserted into it.
struct SeedTable {
    table: &'static str,
    create: &'static str,
    rows: &'static [&'static str],
}

/// The tables every source of the layout table carries: one holding rows and one holding
/// none, so the export is read over a table with data and a table without.
const SEEDED: &[SeedTable] = &[
    SeedTable {
        table: "ledger",
        create: "CREATE TABLE ledger (id INTEGER, label VARCHAR(20))",
        rows: &[
            "INSERT INTO ledger VALUES (1, 'north')",
            "INSERT INTO ledger VALUES (2, 'south')",
        ],
    },
    SeedTable {
        table: "audit",
        create: "CREATE TABLE audit (note TEXT)",
        rows: &[],
    },
];

/// The columns of the exact-numeric table: the declared type, and the Parquet type and
/// `(precision, scale)` the export has to carry it as.
///
/// The exact types are the ones the engines publish as synonyms — "DEC, NUMERIC, and FIXED
/// are synonyms for DECIMAL", and `numeric` is PostgreSQL's own spelling — with and without
/// declared arguments, at the published bound of a Parquet decimal (38 digits), one past it,
/// and with the approximate and integer types beside them as the controls.
struct NumericColumn {
    column: &'static str,
    declared: &'static str,
    /// The Parquet physical type the column is written as.
    physical: PhysicalType,
    /// The converted type a reader reads its bytes through.
    converted: ConvertedType,
    /// `(precision, scale)`, zero for a column that is no decimal.
    precision_and_scale: (i32, i32),
    /// The value the one seeded row holds in this column, as the source's own literal.
    literal: &'static str,
    /// The UNSCALED integer a decimal column must carry that value as.
    unscaled: i128,
}

const NUMERIC_COLUMNS: &[NumericColumn] = &[
    NumericColumn {
        column: "price",
        declared: "DECIMAL(10,2)",
        physical: PhysicalType::FIXED_LEN_BYTE_ARRAY,
        converted: ConvertedType::DECIMAL,
        precision_and_scale: (10, 2),
        literal: "10.57",
        unscaled: 1057,
    },
    NumericColumn {
        column: "score",
        declared: "NUMERIC(12,4)",
        physical: PhysicalType::FIXED_LEN_BYTE_ARRAY,
        converted: ConvertedType::DECIMAL,
        precision_and_scale: (12, 4),
        literal: "-3.1416",
        unscaled: -31_416,
    },
    NumericColumn {
        column: "quantity",
        declared: "DEC",
        physical: PhysicalType::FIXED_LEN_BYTE_ARRAY,
        converted: ConvertedType::DECIMAL,
        // No arguments: the engines' own default for one, `DECIMAL(10, 0)`.
        precision_and_scale: (10, 0),
        literal: "42",
        unscaled: 42,
    },
    NumericColumn {
        column: "widest",
        declared: "FIXED(38,10)",
        physical: PhysicalType::FIXED_LEN_BYTE_ARRAY,
        converted: ConvertedType::DECIMAL,
        // The published bound of a Parquet decimal: 38 digits. Its unscaled value here is
        // past what a 64-bit integer holds, so only the wide decimal carries it.
        precision_and_scale: (38, 10),
        literal: "1234567890.5",
        unscaled: 12_345_678_905_000_000_000,
    },
    NumericColumn {
        column: "duty",
        // An engine's own spelling of the same type, carrying an attribute beside the
        // arguments: the type and its digits are read whatever sits around them.
        declared: "UNSIGNED DECIMAL(8,3)",
        physical: PhysicalType::FIXED_LEN_BYTE_ARRAY,
        converted: ConvertedType::DECIMAL,
        precision_and_scale: (8, 3),
        literal: "4.125",
        unscaled: 4_125,
    },
    NumericColumn {
        column: "beyond",
        declared: "DECIMAL(39,2)",
        // One digit past that bound: the column is exported as its DIGITS, which hold every
        // one of them, rather than as a decimal that would drop some.
        physical: PhysicalType::BYTE_ARRAY,
        converted: ConvertedType::UTF8,
        precision_and_scale: (0, 0),
        literal: "'1.25'",
        unscaled: 0,
    },
    NumericColumn {
        column: "ratio",
        declared: "DOUBLE",
        physical: PhysicalType::DOUBLE,
        converted: ConvertedType::NONE,
        precision_and_scale: (0, 0),
        literal: "0.5",
        unscaled: 0,
    },
    NumericColumn {
        column: "counter",
        declared: "INTEGER",
        physical: PhysicalType::INT64,
        converted: ConvertedType::NONE,
        precision_and_scale: (0, 0),
        literal: "7",
        unscaled: 0,
    },
];

/// The destination an export writes to: the bucket, the key and the role, all held by the
/// services that own them.
struct Destination {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    bucket: String,
    role_arn: String,
    key_id: String,
    /// The ARN of that same key: what a refusal about the KEY the reference resolves to
    /// names, where one about the reference itself echoes the `key_id` above.
    key_arn: String,
}

/// Stand up one destination: a bucket of this Region, a KMS key and a role the export
/// principal may assume that is granted the object write.
async fn destination(bucket: &str, role_name: &str) -> Destination {
    let (router, engine) = emulator();
    create_bucket(&router, bucket, REGION).await;
    let key = created_key(&router, "the export's encryption key", None).await;
    let role_arn = role(
        &router,
        role_name,
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(bucket)),
    )
    .await;
    Destination {
        router,
        engine,
        bucket: bucket.to_string(),
        role_arn,
        key_id: key.id,
        key_arn: key.arn,
    }
}

/// Seed `tables` into `database`, through the engine the source really runs on — so a
/// snapshot taken of it carries real rows.
fn seed(database: &st::data_plane::sql_engine::InstanceDatabase, tables: &[SeedTable]) {
    database
        .create_database(DATABASE)
        .expect("the source's database is created");
    for table in tables {
        database
            .execute(DATABASE, table.create, &[])
            .unwrap_or_else(|error| panic!("seeding {}: {}", table.table, error.message));
        for row in table.rows {
            database
                .execute(DATABASE, row, &[])
                .unwrap_or_else(|error| panic!("seeding {}: {}", table.table, error.message));
        }
    }
}

/// Create a DB instance, poll it to `available`, seed it and answer the engine database it
/// runs on.
async fn available_instance(destination: &Destination, case: &str, identifier: &str) {
    expect_ok(
        case,
        call(
            &destination.router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => "mysql",
                "DBInstanceClass" => "db.t3.micro",
                "AllocatedStorage" => 20,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let request = params!["DBInstanceIdentifier" => identifier];
    poll_until(
        case,
        &format!("the DB instance {identifier} reaching available"),
        || call(&destination.router, "DescribeDBInstances", &request),
        |body| text(body, "DBInstanceStatus") == "available",
    )
    .await;
}

/// The same for a DB cluster.
async fn available_cluster(destination: &Destination, case: &str, identifier: &str) {
    expect_ok(
        case,
        call(
            &destination.router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => "aurora-mysql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let request = params!["DBClusterIdentifier" => identifier];
    poll_until(
        case,
        &format!("the DB cluster {identifier} reaching available"),
        || call(&destination.router, "DescribeDBClusters", &request),
        |body| text(body, "Status") == "available",
    )
    .await;
}

/// The engine database a DB instance runs on.
fn instance_database(
    destination: &Destination,
    identifier: &str,
) -> Arc<st::data_plane::sql_engine::InstanceDatabase> {
    let state = st::service_state(&*destination.engine).expect("this service's store");
    let database = state
        .lock()
        .engine_endpoint(&(REGION.to_string(), identifier.to_string()))
        .map(st::DbEngineEndpoint::database);
    database.expect("a MySQL DB instance serves an engine endpoint")
}

/// The engine database a DB cluster's own volume holds.
fn cluster_database(
    destination: &Destination,
    identifier: &str,
) -> Arc<st::data_plane::sql_engine::InstanceDatabase> {
    let state = st::service_state(&*destination.engine).expect("this service's store");
    let database = state
        .lock()
        .cluster_engine_endpoint(&(REGION.to_string(), identifier.to_string()))
        .map(st::DbEngineEndpoint::database);
    database.expect("an Aurora MySQL DB cluster serves an engine endpoint")
}

/// Take a DB snapshot of `instance` and poll it to `available`, answering its ARN.
async fn available_snapshot(destination: &Destination, case: &str, instance: &str, snapshot: &str) -> String {
    expect_ok(
        case,
        call(
            &destination.router,
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
        case,
        &format!("the DB snapshot {snapshot} reaching available"),
        || call(&destination.router, "DescribeDBSnapshots", &request),
        |body| text(body, "Status") == "available",
    )
    .await;
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}")
}

/// The same for a DB cluster snapshot.
async fn available_cluster_snapshot(
    destination: &Destination,
    case: &str,
    cluster: &str,
    snapshot: &str,
) -> String {
    expect_ok(
        case,
        call(
            &destination.router,
            "CreateDBClusterSnapshot",
            &params![
                "DBClusterIdentifier" => cluster,
                "DBClusterSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    let request = params!["DBClusterSnapshotIdentifier" => snapshot];
    poll_until(
        case,
        &format!("the DB cluster snapshot {snapshot} reaching available"),
        || call(&destination.router, "DescribeDBClusterSnapshots", &request),
        |body| text(body, "Status") == "available",
    )
    .await;
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster-snapshot:{snapshot}")
}

/// Start one export, as a client starts it — against the destination's own bucket and key.
async fn start_export(
    destination: &Destination,
    identifier: &str,
    source_arn: &str,
    prefix: Option<&str>,
) -> (u16, String) {
    let bucket = destination.bucket.clone();
    let key_id = destination.key_id.clone();
    start_export_naming(destination, identifier, source_arn, prefix, &bucket, &key_id).await
}

/// The same call with the destination the request NAMES spelled out, so one row may name a
/// bucket or a key other than the destination's own and change nothing else about the
/// request.
async fn start_export_naming(
    destination: &Destination,
    identifier: &str,
    source_arn: &str,
    prefix: Option<&str>,
    bucket: &str,
    key_id: &str,
) -> (u16, String) {
    let mut request: Vec<(&str, String)> = params![
        "ExportTaskIdentifier" => identifier,
        "SourceArn" => source_arn,
        "S3BucketName" => bucket,
        "IamRoleArn" => destination.role_arn,
        "KmsKeyId" => key_id,
    ]
    .to_vec();
    if let Some(prefix) = prefix {
        request.push(("S3Prefix", prefix.to_string()));
    }
    call(&destination.router, "StartExportTask", &request).await
}

/// The keys the destination bucket holds under `prefix`, through Amazon S3's own listing.
async fn listed_keys(destination: &Destination, prefix: &str) -> Vec<String> {
    s3(&destination.router)
        .list_objects_v2()
        .bucket(&destination.bucket)
        .prefix(prefix)
        .send()
        .await
        .expect("Amazon S3 lists the destination bucket")
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// The bytes Amazon S3 holds at `key`.
async fn object_bytes(destination: &Destination, key: &str) -> Vec<u8> {
    let answer = s3(&destination.router)
        .get_object()
        .bucket(&destination.bucket)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Amazon S3 holds the exported object {key}: {error}"));
    answer
        .body
        .collect()
        .await
        .expect("the exported object's bytes")
        .to_vec()
}

/// One row of the source-kind table: what the export reads and how its source is provisioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// `arn:aws:rds:…:snapshot:<id>` — a DB snapshot of a DB instance.
    DbSnapshot,
    /// `arn:aws:rds:…:cluster-snapshot:<id>` — a DB cluster snapshot.
    ClusterSnapshot,
    /// `arn:aws:rds:…:cluster:<id>` — the DB cluster itself.
    Cluster,
}

/// Rule: an export writes the SOURCE's own tables as Parquet objects under the export's
/// documented path, for every kind of source `SourceArn` may name — each of them carries the
/// data the export reads.
#[tokio::test]
async fn an_export_writes_a_parquet_object_for_every_table_of_every_source_kind() {
    const ROWS: &[(&str, SourceKind, Option<&str>)] = &[
        ("a DB snapshot, at the root of the bucket", SourceKind::DbSnapshot, None),
        ("a DB cluster snapshot, under a prefix", SourceKind::ClusterSnapshot, Some("nightly/exports")),
        ("a DB cluster, under a prefix", SourceKind::Cluster, Some("live")),
    ];

    for (index, (case, kind, prefix)) in ROWS.iter().enumerate() {
        let destination = destination(
            &format!("emu-layout-bucket-{index}"),
            &format!("emu-layout-role-{index}"),
        )
        .await;
        let source_arn = match kind {
            SourceKind::DbSnapshot => {
                let instance = "emu-layout-instance";
                available_instance(&destination, case, instance).await;
                seed(&instance_database(&destination, instance), SEEDED);
                available_snapshot(&destination, case, instance, "emu-layout-snapshot").await
            }
            SourceKind::ClusterSnapshot => {
                let cluster = "emu-layout-cluster-snap";
                available_cluster(&destination, case, cluster).await;
                seed(&cluster_database(&destination, cluster), SEEDED);
                available_cluster_snapshot(&destination, case, cluster, "emu-layout-cluster-snapshot")
                    .await
            }
            SourceKind::Cluster => {
                let cluster = "emu-layout-cluster";
                available_cluster(&destination, case, cluster).await;
                seed(&cluster_database(&destination, cluster), SEEDED);
                format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{cluster}")
            }
        };

        let identifier = format!("emu-layout-export-{index}");
        expect_ok(
            case,
            start_export(&destination, &identifier, &source_arn, *prefix).await,
        );
        let export_prefix = st::export_prefix(*prefix, &identifier);
        let keys = listed_keys(&destination, &export_prefix).await;
        for table in SEEDED {
            let table_prefix = format!("{export_prefix}{DATABASE}/{DATABASE}.{}/", table.table);
            let written: Vec<&String> = keys
                .iter()
                .filter(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet"))
                .collect();
            assert_eq!(
                written.len(),
                1,
                "case {case:?}: expected one Parquet object under {table_prefix}, got {keys:?}"
            );
            // And it is a real Parquet file carrying exactly the rows the source holds.
            let bytes = object_bytes(&destination, written[0]).await;
            let reader = SerializedFileReader::new(Bytes::from(bytes))
                .unwrap_or_else(|error| panic!("case {case:?}: {table_prefix} is Parquet: {error}"));
            let rows = reader.metadata().file_metadata().num_rows();
            assert_eq!(
                rows,
                table.rows.len() as i64,
                "case {case:?}: expected the source's own rows in {table_prefix}, got {rows}"
            );
        }
        // Beside the data, the two JSON files an export writes. Every source here is a
        // MySQL-family one, so what it wrote is the seeded tables plus the server's own
        // stored schema ([`crate::snapshot_export_server_schema`]).
        let written_tables = SEEDED.len() + MYSQL_SERVER_TABLES.len();
        let tables_info = st::tables_info_key(&export_prefix, &identifier, written_tables);
        let export_info = st::export_info_key(&export_prefix, &identifier);
        for expected in [&tables_info, &export_info] {
            assert!(
                keys.contains(expected),
                "case {case:?}: expected the export's {expected} among {keys:?}"
            );
        }
    }
}

/// Rule: a completed export writes its own summary object beside the per-table files, and
/// that summary describes THIS export: what it read, where it wrote, the status it ended in
/// and the rows each table contributed.
///
/// The rows span a prefix given and omitted, and an export limited to one table, which is
/// what the summary's contents generalise over.
#[tokio::test]
async fn the_export_summary_object_describes_the_export_that_ran() {
    let destination = destination("emu-summary-bucket", "emu-summary-role").await;
    let instance = "emu-summary-instance";
    available_instance(&destination, "the export's source", instance).await;
    seed(&instance_database(&destination, instance), SEEDED);
    let source_arn =
        available_snapshot(&destination, "the export's source", instance, "emu-summary-snapshot")
            .await;

    const ROWS: &[(&str, Option<&str>, Option<&str>)] = &[
        ("the whole snapshot at the root of the bucket", None, None),
        ("the whole snapshot under a prefix", Some("exports/nightly"), None),
        ("one table of the snapshot", None, Some("appdb.ledger")),
    ];
    for (index, (case, prefix, export_only)) in ROWS.iter().enumerate() {
        let identifier = format!("emu-summary-export-{index}");
        let mut request: Vec<(&str, String)> = params![
            "ExportTaskIdentifier" => identifier,
            "SourceArn" => source_arn,
            "S3BucketName" => destination.bucket,
            "IamRoleArn" => destination.role_arn,
            "KmsKeyId" => destination.key_id,
        ]
        .to_vec();
        if let Some(prefix) = prefix {
            request.push(("S3Prefix", (*prefix).to_string()));
        }
        if let Some(entry) = export_only {
            request.push(("ExportOnly.member.1", (*entry).to_string()));
        }
        expect_ok(
            case,
            call(&destination.router, "StartExportTask", &request).await,
        );

        let export_prefix = st::export_prefix(*prefix, &identifier);
        let key = st::export_info_key(&export_prefix, &identifier);
        let keys = listed_keys(&destination, &export_prefix).await;
        assert!(
            keys.contains(&key),
            "case {case:?}: expected the export's summary at {key}, got {keys:?}"
        );
        let document: Value = serde_json::from_slice(&object_bytes(&destination, &key).await)
            .unwrap_or_else(|error| panic!("case {case:?}: the summary must parse: {error}"));
        for (member, expected) in [
            ("exportTaskIdentifier", Value::String(identifier.clone())),
            ("sourceArn", Value::String(source_arn.clone())),
            (
                "sourceType",
                Value::String(st::EXPORT_SOURCE_TYPE_SNAPSHOT.to_string()),
            ),
            (
                "s3Bucket",
                Value::String(destination.bucket.clone()),
            ),
            (
                "s3Prefix",
                Value::String(prefix.unwrap_or_default().to_string()),
            ),
            (
                "status",
                Value::String(st::EXPORT_TASK_STATUS_COMPLETE.to_string()),
            ),
        ] {
            assert_eq!(
                document.get(member),
                Some(&expected),
                "case {case:?}: expected {member} to be {expected}, got {document}"
            );
        }
        // One entry per table the export really wrote, each naming the rows it carried: the
        // seeded tables, plus the MySQL server's own stored schema for an export that covers
        // the whole source.
        let exported: Vec<&str> = match export_only {
            Some(entry) => vec![entry.rsplit('.').next().expect("a table name")],
            None => SEEDED
                .iter()
                .map(|table| table.table)
                .chain(MYSQL_SERVER_TABLES)
                .collect(),
        };
        let tables = document
            .get("tables")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: the summary lists its tables, got {document}"));
        assert_eq!(
            tables.len(),
            exported.len(),
            "case {case:?}: expected one entry per exported table, got {document}"
        );
        for table in &exported {
            let entry = tables
                .iter()
                .find(|entry| entry.get("table") == Some(&Value::String((*table).to_string())))
                .unwrap_or_else(|| panic!("case {case:?}: expected {table} in {document}"));
            // The rows a SEEDED table contributed; a table of the server's own schema holds
            // none of this suite's rows, and its own entry above is what it is asserted by.
            let Some(seeded) = SEEDED.iter().find(|seeded| seeded.table == *table) else {
                continue;
            };
            assert_eq!(
                entry.get("rowsExported").and_then(Value::as_u64),
                Some(seeded.rows.len() as u64),
                "case {case:?}: expected the rows {table} holds, got {entry}"
            );
        }
    }
}

/// Rule: a column the source declared an EXACT numeric type is exported as a Parquet DECIMAL
/// of that column's precision and scale, and its values survive as the exact unscaled
/// integers that decimal carries; a column of an approximate or integer type keeps its own
/// Parquet type.
#[tokio::test]
async fn an_exact_numeric_column_is_exported_as_a_decimal_of_its_own_precision_and_scale() {
    let destination = destination("emu-decimal-bucket", "emu-decimal-role").await;
    let instance = "emu-decimal-instance";
    available_instance(&destination, "the decimal source", instance).await;
    {
        let database = instance_database(&destination, instance);
        database
            .create_database(DATABASE)
            .expect("the source's database is created");
        let columns: Vec<String> = NUMERIC_COLUMNS
            .iter()
            .map(|column| format!("{} {}", column.column, column.declared))
            .collect();
        database
            .execute(
                DATABASE,
                &format!("CREATE TABLE amounts ({})", columns.join(", ")),
                &[],
            )
            .unwrap_or_else(|error| panic!("creating amounts: {}", error.message));
        let literals: Vec<&str> = NUMERIC_COLUMNS.iter().map(|column| column.literal).collect();
        database
            .execute(
                DATABASE,
                &format!("INSERT INTO amounts VALUES ({})", literals.join(", ")),
                &[],
            )
            .unwrap_or_else(|error| panic!("seeding amounts: {}", error.message));
    }
    let source_arn = available_snapshot(
        &destination,
        "the decimal source",
        instance,
        "emu-decimal-snapshot",
    )
    .await;

    let identifier = "emu-decimal-export";
    expect_ok(
        "export the decimal table",
        start_export(&destination, identifier, &source_arn, None).await,
    );
    let export_prefix = st::export_prefix(None, identifier);
    let keys = listed_keys(&destination, &export_prefix).await;
    let key = keys
        .iter()
        .find(|key| key.contains(&format!("{DATABASE}.amounts/")))
        .unwrap_or_else(|| panic!("the amounts table was exported, got {keys:?}"))
        .clone();
    let bytes = object_bytes(&destination, &key).await;
    let reader = SerializedFileReader::new(Bytes::from(bytes))
        .unwrap_or_else(|error| panic!("the exported object is Parquet: {error}"));
    let schema = reader.metadata().file_metadata().schema_descr();
    let row = reader
        .get_row_iter(None)
        .expect("the exported rows read")
        .next()
        .expect("the one seeded row")
        .expect("the one seeded row reads");

    for (index, column) in NUMERIC_COLUMNS.iter().enumerate() {
        let case = column.column;
        let described = schema.column(index);
        assert_eq!(
            described.name(),
            column.column,
            "case {case:?}: expected the source's own column order, got {}",
            described.name()
        );
        assert_eq!(
            described.physical_type(),
            column.physical,
            "case {case:?}: expected the Parquet type {:?}, got {:?}",
            column.physical,
            described.physical_type()
        );
        assert_eq!(
            described.converted_type(),
            column.converted,
            "case {case:?}: expected the converted type {:?}, got {:?}",
            column.converted,
            described.converted_type()
        );
        if column.converted != ConvertedType::DECIMAL {
            continue;
        }
        assert_eq!(
            (described.type_precision(), described.type_scale()),
            column.precision_and_scale,
            "case {case:?}: expected the declared precision and scale {:?}, got ({}, {})",
            column.precision_and_scale,
            described.type_precision(),
            described.type_scale()
        );
        // The VALUE is the exact unscaled integer that decimal carries, so nothing of it was
        // rounded into a floating-point type.
        let decimal = row
            .get_decimal(index)
            .unwrap_or_else(|error| panic!("case {case:?}: the column holds a decimal: {error}"));
        let unscaled = bigdecimal::num_bigint::BigInt::from_signed_bytes_be(decimal.data());
        assert_eq!(
            unscaled.to_string(),
            column.unscaled.to_string(),
            "case {case:?}: expected the unscaled value {}, got {unscaled}",
            column.unscaled
        );
        assert_eq!(
            (decimal.precision(), decimal.scale()),
            column.precision_and_scale,
            "case {case:?}: expected the value's own precision and scale {:?}, got ({}, {})",
            column.precision_and_scale,
            decimal.precision(),
            decimal.scale()
        );
    }
}

/// One row of the refusal table: what the request names, and the status, code and sentence
/// the service answers it with.
struct RefusalRow {
    case: &'static str,
    /// The destination bucket the request names, defaulting to the fixture's own.
    bucket: Option<String>,
    /// The role ARN the request names, defaulting to the fixture's own.
    role_arn: Option<String>,
    /// The KMS key the request names, defaulting to the fixture's own.
    key_id: Option<String>,
    status: u16,
    code: &'static str,
    message: String,
}

/// Rule: every refusal of an export carries the status, the error code and the sentence
/// Amazon RDS words that error class in, with the request's own values — and only those —
/// in its variable parts.
///
/// The rows span the two conditions `InvalidS3BucketFault` covers (a bucket that does not
/// exist and one whose name is no bucket name), the bucket outside the export's Region that
/// is refused as the parameter value it is, a role IAM holds nothing under — which is a
/// principal the export's KMS grant cannot be created for, so it carries the KEY's fault —
/// the role whose trust policy admits another principal that `IamRoleNotFound` covers, and
/// the two `KMSKeyNotAccessibleFault` covers for the key itself (a key KMS holds nothing
/// under and one whose own policy authorizes the export role for nothing), each with its own
/// bucket, role and key so the variable parts differ from row to row.
#[tokio::test]
async fn each_export_refusal_carries_the_services_own_sentence() {
    let destination = destination("emu-refusal-bucket", "emu-refusal-role").await;
    let instance = "emu-refusal-instance";
    available_instance(&destination, "the refused exports' source", instance).await;
    seed(&instance_database(&destination, instance), SEEDED);
    let source_arn = available_snapshot(
        &destination,
        "the refused exports' source",
        instance,
        "emu-refusal-snapshot",
    )
    .await;

    // A bucket that exists, in another Region: a destination this export cannot write to.
    let elsewhere = "emu-refusal-elsewhere";
    create_bucket(&destination.router, elsewhere, OTHER_REGION).await;
    // A role IAM holds nothing under, and one it holds that trusts another service.
    let absent = format!("arn:aws:iam::{ACCOUNT}:role/emu-refusal-absent");
    let untrusting = role(
        &destination.router,
        "emu-refusal-untrusting",
        st::RDS_SERVICE_PRINCIPAL,
        Some(export_role_policy(&destination.bucket)),
    )
    .await;
    // A key whose own policy grants the export role nothing: in KMS no principal holds a
    // permission the key policy does not give it.
    let other_role = role(
        &destination.router,
        "emu-refusal-key-owner",
        st::EXPORT_SERVICE_PRINCIPAL,
        None,
    )
    .await;
    let closed_key = created_key(
        &destination.router,
        "a key whose policy decides only about another role",
        Some(key_policy_allowing(&other_role, r#""kms:*""#)),
    )
    .await
    .id;

    let rows = [
        RefusalRow {
            case: "a destination bucket Amazon S3 holds nothing under",
            bucket: Some("emu-refusal-no-such-bucket".to_string()),
            role_arn: None,
            key_id: None,
            status: 400,
            code: "InvalidS3BucketFault",
            message: "The S3 bucket emu-refusal-no-such-bucket isn't valid. Either it isn't \
                      located in the current AWS Region or it doesn't exist."
                .to_string(),
        },
        RefusalRow {
            case: "a destination bucket name that is no bucket name",
            bucket: Some("emu..refusal".to_string()),
            role_arn: None,
            key_id: None,
            status: 400,
            code: "InvalidS3BucketFault",
            message: "The S3 bucket emu..refusal isn't valid. Either it isn't located in the \
                      current AWS Region or it doesn't exist."
                .to_string(),
        },
        RefusalRow {
            case: "a destination bucket in another Region",
            bucket: Some(elsewhere.to_string()),
            role_arn: None,
            key_id: None,
            status: 400,
            code: "InvalidParameterValue",
            message: format!("The S3 bucket {elsewhere} isn't located in the current AWS Region."),
        },
        RefusalRow {
            // The export's KMS grant is created FOR the role the request names, so a role
            // IAM holds nothing under is a grant that cannot be created: recorded behaviour
            // answers that with the key fault in the generic "specified KMS key" wording —
            // naming the KEY the request's reference resolves to, and neither the role nor
            // the action — where a key that is itself unusable carries the other sentence two
            // rows down, about the reference the request carried.
            case: "a role IAM holds nothing under",
            bucket: None,
            role_arn: Some(absent.clone()),
            key_id: None,
            status: 400,
            code: "KMSKeyNotAccessibleFault",
            message: format!(
                "The specified KMS key [{}] does not exist, is not enabled or you do not \
                 have permissions to access it.",
                destination.key_arn
            ),
        },
        RefusalRow {
            case: "a role whose trust policy admits another service principal",
            bucket: None,
            role_arn: Some(untrusting.clone()),
            key_id: None,
            status: 404,
            code: "IamRoleNotFound",
            message: format!(
                "The principal {} isn't allowed to assume the IAM role {untrusting} or the \
                 IAM role {untrusting} doesn't exist.",
                st::EXPORT_SERVICE_PRINCIPAL
            ),
        },
        RefusalRow {
            case: "a KMS key whose policy authorizes the export role for nothing",
            bucket: None,
            role_arn: None,
            key_id: Some(closed_key.clone()),
            status: 400,
            code: "KMSKeyNotAccessibleFault",
            message: format!("The KMS key {closed_key} doesn't exist or is disabled."),
        },
        RefusalRow {
            case: "a KMS key KMS holds nothing under",
            bucket: None,
            role_arn: None,
            key_id: Some(format!(
                "arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-3333-4444-555555555555"
            )),
            status: 400,
            code: "KMSKeyNotAccessibleFault",
            message: format!(
                "The KMS key arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-3333-4444-\
                 555555555555 doesn't exist or is disabled."
            ),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let body = expect_error(
            row.case,
            call(
                &destination.router,
                "StartExportTask",
                &params![
                    "ExportTaskIdentifier" => format!("emu-refusal-export-{index}"),
                    "SourceArn" => source_arn,
                    "S3BucketName" => row.bucket.clone().unwrap_or_else(|| destination.bucket.clone()),
                    "IamRoleArn" => row.role_arn.clone().unwrap_or_else(|| destination.role_arn.clone()),
                    "KmsKeyId" => row.key_id.clone().unwrap_or_else(|| destination.key_id.clone()),
                ],
            )
            .await,
            row.status,
            row.code,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, row.message,
            "case {:?}: expected the refusal sentence {:?}, got {message:?}",
            row.case, row.message
        );
    }

    // And nothing of a refused export was written or recorded.
    let keys = listed_keys(&destination, "").await;
    assert!(
        keys.is_empty(),
        "a refused export writes no object, got {keys:?}"
    );
}

/// One row of the unusable-key table: how the request spells the key, and what leaves that
/// key unable to encrypt the export.
struct UnusableKeyRow {
    case: &'static str,
    /// What makes the key unusable; the key reference the request carries is built from it.
    defect: KeyDefect,
}

/// The conditions that leave the key named by `KmsKeyId` unable to encrypt an export.
enum KeyDefect {
    /// KMS holds no key under the reference the request carries, which is this value.
    Absent(String),
    /// KMS holds the key but it is not enabled; the request names it by its key id, or by
    /// its key ARN when `by_arn`.
    Disabled { by_arn: bool },
    /// KMS holds the key enabled, and its own policy does not authorize the export's role
    /// for both of the operations the export performs on it. The document is the key's
    /// policy; the request names the key by its id, or by its ARN when `by_arn`.
    Policy { document: String, by_arn: bool },
}

/// A key policy whose ONE statement decides about `principal`: the form a key's own policy
/// takes when the account is not granted at all ("no AWS principal has any permissions to a
/// KMS key unless the key policy allows it"), so a principal the statement does not cover is
/// authorized for nothing.
fn key_policy_allowing(principal: &str, actions: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":{actions},"Resource":"*"}}]}}"#
    )
}

/// A key policy that GRANTS the account — the default key policy's own statement, which
/// delegates to IAM — and then denies `principal` the action `action` on the key: "an
/// explicit deny in any policy overrides any allow", so the deny is what decides.
fn key_policy_denying(principal: &str, action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"kms:*","Resource":"*"}},{{"Effect":"Deny","Principal":{{"AWS":"{principal}"}},"Action":"{action}","Resource":"*"}}]}}"#
    )
}

/// Rule: every condition that leaves `KmsKeyId` unable to encrypt an export — a key KMS
/// holds nothing under, a key it holds disabled, and a key whose own policy withholds either
/// of the two KMS operations the export performs on it — is refused with the one modelled
/// `KMSKeyNotAccessibleFault` (HTTP 400) and the single sentence that fault carries here,
/// whose only variable part is the key reference echoed exactly as the request spelled it.
/// Neither the role the export runs under nor the KMS action that was withheld is named, and
/// no export task is recorded and no object written.
///
/// The rows span the three conditions, the spellings `KmsKeyId` accepts ("the key ARN, key
/// ID, alias ARN, or alias name") and — across the policy rows — which of the two actions
/// (`kms:CreateGrant` and `kms:DescribeKey`, as the member lists them) is the one withheld,
/// so a sentence naming the action that failed could not pass.
#[tokio::test]
async fn every_condition_that_makes_the_export_key_unusable_carries_the_one_key_sentence() {
    let destination = destination("emu-key-fault-bucket", "emu-key-fault-role").await;
    let instance = "emu-key-fault-instance";
    available_instance(&destination, "the unusable keys' source", instance).await;
    seed(&instance_database(&destination, instance), SEEDED);
    let source_arn = available_snapshot(
        &destination,
        "the unusable keys' source",
        instance,
        "emu-key-fault-snapshot",
    )
    .await;
    // Another principal, for the policy rows that decide about someone else: a key that
    // authorizes a different role authorizes this export for nothing.
    let other_role = role(
        &destination.router,
        "emu-key-fault-other",
        st::EXPORT_SERVICE_PRINCIPAL,
        None,
    )
    .await;
    let export_role = destination.role_arn.clone();

    let rows = [
        UnusableKeyRow {
            case: "a key ARN KMS holds nothing under",
            defect: KeyDefect::Absent(format!(
                "arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-3333-4444-555555555555"
            )),
        },
        UnusableKeyRow {
            case: "a bare key id KMS holds nothing under",
            defect: KeyDefect::Absent("22222222-3333-4444-5555-666666666666".to_string()),
        },
        UnusableKeyRow {
            case: "an alias this account holds no key under",
            defect: KeyDefect::Absent("alias/emu-key-fault-absent".to_string()),
        },
        UnusableKeyRow {
            case: "a key KMS holds disabled, named by its key id",
            defect: KeyDefect::Disabled { by_arn: false },
        },
        UnusableKeyRow {
            case: "a key KMS holds disabled, named by its key ARN",
            defect: KeyDefect::Disabled { by_arn: true },
        },
        UnusableKeyRow {
            case: "a key whose policy decides only about another principal",
            defect: KeyDefect::Policy {
                document: key_policy_allowing(&other_role, r#""kms:*""#),
                by_arn: false,
            },
        },
        UnusableKeyRow {
            case: "a key whose policy denies the export role kms:CreateGrant",
            defect: KeyDefect::Policy {
                document: key_policy_denying(&export_role, "kms:CreateGrant"),
                by_arn: false,
            },
        },
        UnusableKeyRow {
            case: "a key whose policy denies the export role kms:DescribeKey",
            defect: KeyDefect::Policy {
                document: key_policy_denying(&export_role, "kms:DescribeKey"),
                by_arn: true,
            },
        },
        UnusableKeyRow {
            case: "a key whose policy grants the export role only kms:DescribeKey",
            defect: KeyDefect::Policy {
                document: key_policy_allowing(&export_role, r#""kms:DescribeKey""#),
                by_arn: false,
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let named = match &row.defect {
            KeyDefect::Absent(reference) => reference.clone(),
            KeyDefect::Disabled { by_arn } => {
                let key = created_key(&destination.router, row.case, None).await;
                kms(&destination.router)
                    .disable_key()
                    .key_id(&key.id)
                    .send()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("case {:?}: the emulated KMS disables the key: {error}", row.case)
                    });
                key.named(*by_arn)
            }
            KeyDefect::Policy { document, by_arn } => {
                created_key(&destination.router, row.case, Some(document.clone()))
                    .await
                    .named(*by_arn)
            }
        };
        let body = expect_error(
            row.case,
            call(
                &destination.router,
                "StartExportTask",
                &params![
                    "ExportTaskIdentifier" => format!("emu-key-fault-export-{index}"),
                    "SourceArn" => source_arn,
                    "S3BucketName" => destination.bucket,
                    "IamRoleArn" => destination.role_arn,
                    "KmsKeyId" => named,
                ],
            )
            .await,
            400,
            "KMSKeyNotAccessibleFault",
        );
        let expected = format!("The KMS key {named} doesn't exist or is disabled.");
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {:?}: expected the refusal sentence {expected:?}, got {message:?}",
            row.case
        );
    }

    // A refused export records no task and writes no object: the refusal is a decision about
    // the request, so nothing of it survives anywhere.
    let described = expect_ok(
        "no task was recorded",
        call(
            &destination.router,
            "DescribeExportTasks",
            &params!["MaxRecords" => 20],
        )
        .await,
    );
    assert!(
        subtrees(&described, "ExportTask").is_empty(),
        "a refused export records no task, got {described}"
    );
    let keys = listed_keys(&destination, "").await;
    assert!(
        keys.is_empty(),
        "a refused export writes no object, got {keys:?}"
    );
}

/// One key this emulator's own KMS holds: both spellings of it a request may carry.
struct CreatedKey {
    id: String,
    arn: String,
}

impl CreatedKey {
    /// The reference a request names this key by: its ARN, or its bare key id.
    fn named(self, by_arn: bool) -> String {
        if by_arn {
            self.arn
        } else {
            self.id
        }
    }
}

/// Create one key through this emulator's own KMS, with `policy` as its key policy when one
/// is given — a key that service really holds, which is what the export resolves. The ONE
/// create-a-key call of this suite: every key it stands up, with a policy or with the default
/// one, is minted here.
async fn created_key(
    router: &MultiServiceRouter,
    case: &str,
    policy: Option<String>,
) -> CreatedKey {
    let mut request = kms(router).create_key().description(case);
    if let Some(policy) = policy {
        // A policy naming a principal other than this account's own calls for the lockout
        // check to be bypassed, exactly as it does against the real service.
        request = request
            .policy(policy)
            .bypass_policy_lockout_safety_check(true);
    }
    let metadata = request
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the emulated KMS creates the key: {error}"))
        .key_metadata
        .unwrap_or_else(|| panic!("case {case:?}: the created key reports its metadata"));
    CreatedKey {
        id: metadata.key_id().to_string(),
        arn: metadata
            .arn()
            .unwrap_or_else(|| panic!("case {case:?}: the created key reports its ARN"))
            .to_string(),
    }
}

/// What leaves ONE resource the request names unusable for the export, and nothing else about
/// the request: the pair of calls a row makes differ in this parameter alone.
enum Unresolvable {
    /// The request names a destination bucket this Region's object store holds nothing under;
    /// this is that name.
    Bucket(&'static str),
    /// The request names a key KMS holds nothing under; this is the reference it carries.
    AbsentKey(&'static str),
    /// KMS holds the key, enabled, and its own policy denies the export's role this KMS
    /// action — one of the two the key's member lists.
    KeyDenying(&'static str),
    /// KMS holds the key, disabled.
    DisabledKey,
}

/// One row of the refused-then-accepted table: what is unresolvable about the first call, the
/// `S3Prefix` both calls carry, and the refusal the first is answered with.
struct PairedRow {
    case: &'static str,
    unresolvable: Unresolvable,
    /// `S3Prefix`, given or omitted — both calls of the pair carry the same one.
    prefix: Option<&'static str>,
    code: &'static str,
}

/// Rule: whether an export is accepted is decided by whether the request's destination bucket
/// and encryption key RESOLVE — a bucket the Region's object store holds nothing under, or a
/// key KMS cannot encrypt under, is refused synchronously (HTTP 400) with that condition's own
/// modelled fault and sentence, records no export task and writes no object; the same request
/// with that one parameter replaced by a resolvable one is accepted, reports `STARTING` with
/// the destination it was given echoed back, and its task then reaches the terminal `COMPLETE`
/// with all of its progress and the export's objects under its own prefix.
///
/// Each row makes the SAME request twice under the same `ExportTaskIdentifier` and the same
/// `S3Prefix`, changing only the one parameter that could not be resolved — so the accept is
/// the control for its own refusal, and it also establishes that the refusal recorded nothing:
/// an identifier a task had been recorded under would be refused `ExportTaskAlreadyExistsFault`
/// instead.
///
/// The rows span which parameter is unresolvable (the destination bucket, the key), the forms
/// that takes for a key (KMS holds nothing under the reference, holds it disabled, holds it
/// with a policy denying each of the two actions the export performs on it) and an `S3Prefix`
/// given and omitted.
///
/// The accepted half is asserted here rather than left to the suites that own the terminal
/// status and the object layout, because it is the CONTROL of its own refusal: what the rule
/// says is that the one parameter decided the outcome, which neither half shows alone.
#[tokio::test]
async fn an_export_refused_for_an_unresolvable_destination_is_accepted_once_it_resolves() {
    let destination = destination("emu-paired-bucket", "emu-paired-role").await;
    let instance = "emu-paired-instance";
    available_instance(&destination, "the paired exports' source", instance).await;
    seed(&instance_database(&destination, instance), SEEDED);
    let source_arn = available_snapshot(
        &destination,
        "the paired exports' source",
        instance,
        "emu-paired-snapshot",
    )
    .await;

    let rows = [
        PairedRow {
            case: "a destination bucket Amazon S3 holds nothing under",
            unresolvable: Unresolvable::Bucket("emu-paired-no-such-bucket"),
            prefix: None,
            code: "InvalidS3BucketFault",
        },
        PairedRow {
            case: "a key reference KMS holds nothing under",
            unresolvable: Unresolvable::AbsentKey("33333333-4444-5555-6666-777777777777"),
            prefix: Some("nightly/exports"),
            code: "KMSKeyNotAccessibleFault",
        },
        PairedRow {
            case: "a key whose policy denies the export role kms:CreateGrant",
            unresolvable: Unresolvable::KeyDenying("kms:CreateGrant"),
            prefix: Some("create-grant-denied"),
            code: "KMSKeyNotAccessibleFault",
        },
        PairedRow {
            case: "a key whose policy denies the export role kms:DescribeKey",
            unresolvable: Unresolvable::KeyDenying("kms:DescribeKey"),
            prefix: Some("deep/nested/describe-key-denied"),
            code: "KMSKeyNotAccessibleFault",
        },
        PairedRow {
            case: "a key KMS holds disabled",
            unresolvable: Unresolvable::DisabledKey,
            prefix: Some("disabled-key-attempt"),
            code: "KMSKeyNotAccessibleFault",
        },
    ];

    // What each accepted export has to end up as, polled once every row has started its own:
    // `(case, identifier, the prefix the export wrote under)`.
    let mut accepted: Vec<(&str, String, String)> = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        // The one parameter that cannot be resolved, and the sentence the refusal carries for
        // it: fixed text, with the value the request named as its only variable part.
        let (bucket, key_id, expected_message) = match &row.unresolvable {
            Unresolvable::Bucket(bucket) => (
                (*bucket).to_string(),
                destination.key_id.clone(),
                format!(
                    "The S3 bucket {bucket} isn't valid. Either it isn't located in the \
                     current AWS Region or it doesn't exist."
                ),
            ),
            Unresolvable::AbsentKey(reference) => (
                destination.bucket.clone(),
                (*reference).to_string(),
                format!("The KMS key {reference} doesn't exist or is disabled."),
            ),
            Unresolvable::KeyDenying(action) => {
                let key = created_key(
                    &destination.router,
                    row.case,
                    Some(key_policy_denying(&destination.role_arn, action)),
                )
                .await
                .id;
                let message = format!("The KMS key {key} doesn't exist or is disabled.");
                (destination.bucket.clone(), key, message)
            }
            Unresolvable::DisabledKey => {
                let key = created_key(&destination.router, row.case, None).await.id;
                kms(&destination.router)
                    .disable_key()
                    .key_id(&key)
                    .send()
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {:?}: the emulated KMS disables the key: {error}",
                            row.case
                        )
                    });
                let message = format!("The KMS key {key} doesn't exist or is disabled.");
                (destination.bucket.clone(), key, message)
            }
        };

        let identifier = format!("emu-paired-export-{index}");
        let export_prefix = st::export_prefix(row.prefix, &identifier);

        // The request as it stands: refused, with the fault and the sentence of the condition
        // that stopped it.
        let refused = expect_error(
            row.case,
            start_export_naming(
                &destination,
                &identifier,
                &source_arn,
                row.prefix,
                &bucket,
                &key_id,
            )
            .await,
            400,
            row.code,
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected_message,
            "case {:?}: expected the refusal sentence {expected_message:?}, got {message:?}",
            row.case
        );

        // Nothing of the refused request was recorded: no task of that identifier is reported.
        let listed = expect_ok(
            row.case,
            call(
                &destination.router,
                "DescribeExportTasks",
                &params!["MaxRecords" => 100],
            )
            .await,
        );
        let reported = texts(&listed, "ExportTaskIdentifier");
        assert!(
            !reported.contains(&identifier),
            "case {:?}: a refused export records no task, got {reported:?}",
            row.case
        );
        // And nothing of it was written to the destination it would have written to.
        let keys = listed_keys(&destination, &export_prefix).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a refused export writes no object under {export_prefix}, got {keys:?}",
            row.case
        );

        // The SAME request, with the one unresolvable parameter replaced: accepted.
        let started = expect_ok(
            row.case,
            start_export(&destination, &identifier, &source_arn, row.prefix).await,
        );
        for (member, expected) in [
            ("Status", "STARTING".to_string()),
            ("ExportTaskIdentifier", identifier.clone()),
            ("SourceArn", source_arn.clone()),
            ("S3Bucket", destination.bucket.clone()),
            ("IamRoleArn", destination.role_arn.clone()),
            ("KmsKeyId", destination.key_id.clone()),
            // `ExportTask$PercentProgress` and `ExportTask$Status` as the model publishes
            // them, written out here rather than read from this crate's own constants.
            ("PercentProgress", "0".to_string()),
        ] {
            let answered = text(&started, member);
            assert_eq!(
                answered, expected,
                "case {:?}: expected {member} {expected:?} on the accepted export, got \
                 {answered:?} in {started}",
                row.case
            );
        }
        match row.prefix {
            Some(prefix) => assert_eq!(
                text(&started, "S3Prefix"),
                prefix,
                "case {:?}: an accepted export echoes the S3Prefix it was given, got {started}",
                row.case
            ),
            None => assert!(
                subtrees(&started, "S3Prefix").is_empty(),
                "case {:?}: an export given no S3Prefix reports none, got {started}",
                row.case
            ),
        }
        accepted.push((row.case, identifier, export_prefix));
    }

    // Every accepted export walks to its terminal status, and that status is COMPLETE with all
    // of its progress reported and the instant it ended.
    for (case, identifier, export_prefix) in &accepted {
        let request = params!["ExportTaskIdentifier" => identifier];
        let described = poll_until(
            case,
            &format!("the export task {identifier} reaching a terminal status"),
            || call(&destination.router, "DescribeExportTasks", &request),
            // The statuses an export does not leave on its own, as `ExportTask$Status`
            // publishes them: the poll settles on whichever one it ends in, and the
            // assertions below are what decide that it is the right one.
            |body| ["COMPLETE", "FAILED", "CANCELED"].contains(&text(body, "Status").as_str()),
        )
        .await;
        for (member, expected) in [
            ("Status", "COMPLETE".to_string()),
            ("PercentProgress", "100".to_string()),
        ] {
            let answered = text(&described, member);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {member} {expected:?} on the finished export, got \
                 {answered:?} in {described}"
            );
        }
        assert!(
            !texts(&described, "TaskEndTime").is_empty(),
            "case {case:?}: a finished export reports when it ended, got {described}"
        );

        // And the bucket holds the export: one Parquet object per table of the source, and the
        // two JSON files an export leaves beside them.
        let keys = listed_keys(&destination, export_prefix).await;
        for table in SEEDED {
            let table_prefix = format!("{export_prefix}{DATABASE}/{DATABASE}.{}/", table.table);
            assert!(
                keys.iter()
                    .any(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet")),
                "case {case:?}: expected a Parquet object under {table_prefix}, got {keys:?}"
            );
        }
        for expected in [
            st::tables_info_key(
                export_prefix,
                identifier,
                // The whole source: the seeded tables plus the MySQL server's own stored
                // schema, which is part of what the snapshot carries.
                SEEDED.len() + MYSQL_SERVER_TABLES.len(),
            ),
            st::export_info_key(export_prefix, identifier),
        ] {
            assert!(
                keys.contains(&expected),
                "case {case:?}: expected the export's {expected} among {keys:?}"
            );
        }
    }
}
