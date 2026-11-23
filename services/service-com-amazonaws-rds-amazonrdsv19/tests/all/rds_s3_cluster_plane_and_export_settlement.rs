// SPDX-License-Identifier: CC0-1.0

//! The two surfaces an Amazon S3 workflow of Amazon RDS reaches END TO END, driven the way a
//! client drives them: the ENGINE plane a DB cluster publishes, and the snapshot EXPORT a
//! client polls to its terminal status.
//!
//! The rules this suite pins:
//!
//! * The `Endpoint`/`Port` pair an available DB cluster publishes is an address THIS emulator
//!   answers that cluster's engine wire on. `DBCluster$Endpoint` is "the connection endpoint
//!   for the primary instance of the DB cluster" and `DBCluster$Port` "the port that the
//!   database engine is listening on"
//!   (`smithy_query(query="shape",shape="DBCluster")`), so the pair is the whole address a
//!   client composes: a client that reads it out of `DescribeDBClusters`, resolves the host
//!   and dials the port completes the engine's own handshake with the cluster's master
//!   credentials and runs statements on the cluster's database. A host in a zone this
//!   emulator neither serves nor owns would be no connection endpoint at all, whatever a
//!   resolver did with it.
//! * `LOAD DATA FROM S3` issued on that published endpoint loads the named object's records as
//!   rows exactly when the cluster's import role is in force — the role its
//!   `aws_default_s3_role` parameter names AND that the cluster has associated — and the
//!   policies IAM holds for that role allow the object read; a cluster whose role is granted
//!   the read on another bucket loads nothing and is told so by the engine.
//! * A client that only REPORTS reaches the END of the snapshot-export lifecycle: the snapshot
//!   `CreateDBSnapshot` accepted leaves `creating` and settles `available`, the task
//!   `StartExportTask` accepted leaves `STARTING` and settles `COMPLETE` with
//!   `PercentProgress` 100 and a `TaskEndTime`, and the objects it wrote lie under
//!   `<S3Prefix>/<ExportTaskIdentifier>/` in the destination bucket. An export whose role is
//!   not authorized on the bucket it names is refused BEFORE anything is written — HTTP 400
//!   `IamRoleMissingPermissions` carrying the service's own sentence for that class, whose
//!   variable parts are the request's role, the call that failed and the bucket — and the same
//!   role is accepted against the bucket it IS granted on.
//!
//! What this suite adds beside its siblings is the WHOLE CHAIN in one place, driven the way a
//! client drives it: the sibling suites pin each rule on its own surface — the published
//! address and the import at it ([`crate::served_cluster_endpoint_address`],
//! [`crate::mysql_s3_load`]), the task's progression
//! ([`crate::export_task_lifecycle`]) and the refusal's sentence over a broader role table
//! ([`crate::rds_s3_integration`]) — while here one client reads the published pair, dials it,
//! imports through it, seeds the source of an export over the same kind of address, snapshots
//! it, exports it and lists the objects, so a break anywhere along that chain fails here.
//!
//! Amazon S3, IAM and KMS are mounted beside this service on ONE engine, exactly as the
//! assembled router mounts them: the objects an import reads and an export writes are objects
//! that service really holds, the roles are roles IAM holds, and the key is a key KMS holds.
//! The rows below span what the rules generalise over: both halves of the import's
//! authorization, two objects differing in record count and value length, an export with a
//! prefix and without one, and two destination buckets a granted role is not granted on.
//!
//! Every wait here is a poll loop over a REPORTING call
//! ([`crate::lifecycle_poll_loops::poll_until`]), so only a transition this service's clock
//! already owed can end one and no assertion depends on how fast the host runs.

use std::net::SocketAddr;

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_parameter_groups::{create_group, modify};
use crate::db_clusters::the_cluster;
use crate::db_instance_engine_endpoint::{
    endpoint_of, wire_login, Wire, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, expect_error, expect_ok, optional_text, subtrees, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    create_bucket, emulator, export_role_policy, kms, put_object, role, s3,
};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The statuses an export task has STOPPED in — what a poll loop here waits for, taken from
/// the store's own list so this suite and the handler cannot drift on what "stopped" means.
const TERMINAL_EXPORT_STATUSES: [&str; 3] = st::TERMINAL_STATUSES;

/// The MySQL server error number a failure of the engine's OWN Amazon S3 call carries
/// (`ER_S3_API_ERROR`), which is what a client that has just configured an import branches on.
const ER_S3_API_ERROR: &str = "1871";

// ---------------------------------------------------------------------------
// The address a published endpoint is
// ---------------------------------------------------------------------------

/// Assert that `host`:`port` is an address THIS emulator answers on, as a client establishes
/// it: the host resolves, and every address it resolves to is one of this machine's. That the
/// port is ANSWERED is what the session each caller opens next establishes.
///
/// A host under a zone AWS itself serves is refused outright: such a name resolves nowhere
/// near this process — or, worse, at genuine AWS infrastructure — so publishing one would be
/// announcing an address other than the one the resource is reached on.
async fn assert_own_address(case: &str, host: &str, port: u16) {
    assert!(
        port > 0,
        "case {case:?}: expected the published port of a served engine, got {port}"
    );
    for zone in ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
        assert!(
            !host.ends_with(zone),
            "case {case:?}: a resource this emulator serves must publish no host in a zone \
             AWS serves ({zone}), got {host:?}"
        );
    }
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published host {host:?} must resolve, got {error}")
        })
        .collect();
    assert!(
        !resolved.is_empty() && resolved.iter().all(|address| address.ip().is_loopback()),
        "case {case:?}: expected the published host {host:?} to resolve to this machine, got \
         {resolved:?}"
    );
}

// ---------------------------------------------------------------------------
// The import a cluster's published endpoint serves
// ---------------------------------------------------------------------------

/// The bucket the imported objects live in.
const IMPORT_BUCKET: &str = "emu-cpe-import";

/// A second bucket, the one the unauthorized row's role is granted on instead.
const OTHER_BUCKET: &str = "emu-cpe-elsewhere";

/// The identity policy an engine's Amazon S3 IMPORT role carries: the object read the
/// statement performs and the bucket listing beside it, on the bucket and on the objects in
/// it — the grants "Setting up access to an Amazon S3 bucket" (Amazon RDS User Guide) asks
/// for, and the pair the issue's own scenario grants.
fn import_role_policy(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetObject","s3:ListBucket"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}}]}}"#
    )
}

/// One object staged for the import: the table it is loaded into, the object's own bytes, and
/// the rows the table must hold afterwards.
struct ObjectRow {
    case: &'static str,
    table: &'static str,
    key: &'static str,
    body: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

/// The objects the import reads, differing in how many records they carry and how long their
/// values are — what the statement's record reading generalises over.
const OBJECTS: &[ObjectRow] = &[
    ObjectRow {
        case: "an object of two records",
        table: "landing_pairs",
        key: "import/pairs.csv",
        body: "1,alice\n2,bob\n",
        rows: &[("1", "alice"), ("2", "bob")],
    },
    ObjectRow {
        case: "an object of three records, one with a much longer value",
        table: "landing_three",
        key: "import/three.csv",
        body: "10,aurora\n11,mysql\n12,a-rather-longer-customer-name\n",
        rows: &[
            ("10", "aurora"),
            ("11", "mysql"),
            ("12", "a-rather-longer-customer-name"),
        ],
    },
];

/// One cluster of the import table: how its import role is granted, and whether the import it
/// is configured for may therefore read [`IMPORT_BUCKET`]'s objects.
struct ClusterRow {
    case: &'static str,
    identifier: &'static str,
    group: &'static str,
    role_name: &'static str,
    /// The bucket this row's role is granted the object read on.
    granted_bucket: &'static str,
    loads: bool,
}

const CLUSTER_ROWS: &[ClusterRow] = &[
    ClusterRow {
        case: "a cluster whose import role is granted the read on the object's bucket",
        identifier: "emu-cpe-authorized",
        group: "emu-cpe-authorized-cpg",
        role_name: "emu-cpe-authorized-role",
        granted_bucket: IMPORT_BUCKET,
        loads: true,
    },
    ClusterRow {
        case: "a cluster whose import role is granted another bucket only",
        identifier: "emu-cpe-unauthorized",
        group: "emu-cpe-unauthorized-cpg",
        role_name: "emu-cpe-unauthorized-role",
        granted_bucket: OTHER_BUCKET,
        loads: false,
    },
];

/// Create the row's Aurora MySQL cluster with `group` as its DB cluster parameter group, poll
/// it to the status a client dials it in, and answer with the pair it published — asserting
/// along the way that the describe reports the pair the create did and that the pair is an
/// address this emulator answers on.
async fn available_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    group: &str,
) -> (String, u16) {
    let created = the_cluster(
        case,
        &expect_ok(
            case,
            call(
                router,
                "CreateDBCluster",
                &params![
                    "DBClusterIdentifier" => identifier,
                    "Engine" => "aurora-mysql",
                    "DatabaseName" => DATABASE,
                    "MasterUsername" => MASTER_USER,
                    "MasterUserPassword" => MASTER_PASSWORD,
                    "DBClusterParameterGroupName" => group,
                ],
            )
            .await,
        ),
    );
    let published = (text(&created, "Endpoint"), text(&created, "Port"));

    let request = params!["DBClusterIdentifier" => identifier];
    let (body, _polls) = poll_until(
        case,
        "the DB cluster reaching the status a client dials it in",
        || call(router, "DescribeDBClusters", &request),
        |status, body| {
            status == 200
                && subtrees(body, "DBCluster").len() == 1
                && text(&the_cluster(case, body), "Status") == "available"
        },
    )
    .await;
    let settled = the_cluster(case, &body);
    let reported = (text(&settled, "Endpoint"), text(&settled, "Port"));
    assert_eq!(
        reported, published,
        "case {case:?}: expected the describe to report the pair the create published \
         {published:?}, got {reported:?}"
    );
    let port: u16 = published.1.parse().unwrap_or_else(|_| {
        panic!(
            "case {case:?}: expected a port beside the cluster's endpoint, got {:?}",
            published.1
        )
    });
    assert_own_address(case, &published.0, port).await;
    (published.0, port)
}

/// RULE: the pair an available DB cluster publishes is the address this emulator answers its
/// engine wire on, and the Amazon S3 import a client issues there loads the object's records
/// exactly when the cluster's import role is in force and authorized for the read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_published_cluster_endpoint_serves_the_s3_import_its_role_authorizes() {
    let (router, _engine) = emulator();
    create_bucket(&router, IMPORT_BUCKET, REGION).await;
    create_bucket(&router, OTHER_BUCKET, REGION).await;
    for object in OBJECTS {
        put_object(
            &router,
            IMPORT_BUCKET,
            object.key,
            object.body.as_bytes().to_vec(),
        )
        .await;
    }

    for row in CLUSTER_ROWS {
        // The role the engine acts under: named by the cluster's own parameter AND associated
        // with the cluster, which are the two halves AWS asks for.
        let role_arn = role(
            &router,
            row.role_name,
            st::RDS_SERVICE_PRINCIPAL,
            Some(import_role_policy(row.granted_bucket)),
        )
        .await;
        let group = create_group(&router, row.case, row.group, "aurora-mysql8.0").await;
        expect_ok(
            row.case,
            modify(&router, &group, "aws_default_s3_role", &role_arn, None).await,
        );
        let (host, port) = available_cluster(&router, row.case, row.identifier, &group).await;
        expect_ok(
            row.case,
            call(
                &router,
                "AddRoleToDBCluster",
                &params![
                    "DBClusterIdentifier" => row.identifier,
                    "RoleArn" => role_arn,
                ],
            )
            .await,
        );

        // The published pair, dialled as a client dials it: the master credentials are
        // admitted by the engine behind it and the session runs in the cluster's database.
        let mut session = wire_login(row.case, Wire::MySql, &host, port, DATABASE).await;
        let database = session.value(row.case, "SELECT DATABASE()").await;
        assert_eq!(
            database, DATABASE,
            "case {:?}: expected the session at the published endpoint to be in the cluster's \
             database {DATABASE:?}, got {database:?}",
            row.case
        );

        for object in OBJECTS {
            let case = format!("{} / {}", row.case, object.case);
            session
                .run(
                    &case,
                    &format!(
                        "CREATE TABLE {} (id int, name varchar(64))",
                        object.table
                    ),
                )
                .await;
            let statement = format!(
                "LOAD DATA FROM S3 's3://{IMPORT_BUCKET}/{}' INTO TABLE {} FIELDS TERMINATED \
                 BY ','",
                object.key, object.table
            );
            let answer = session.query(&statement).await;
            let read_back = format!("SELECT id, name FROM {} ORDER BY id", object.table);
            if row.loads {
                answer.unwrap_or_else(|refusal| {
                    panic!("case {case:?}: {statement:?} must load, got {refusal:?}")
                });
                let loaded = session.run(&case, &read_back).await.rows;
                let expected: Vec<Vec<Option<String>>> = object
                    .rows
                    .iter()
                    .map(|(id, name)| vec![Some((*id).to_string()), Some((*name).to_string())])
                    .collect();
                assert_eq!(
                    loaded, expected,
                    "case {case:?}: expected the object's own records as rows, got {loaded:?}"
                );
            } else {
                let refusal = answer.err().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: an import whose role is not granted the read must be \
                         refused"
                    )
                });
                // The engine's OWN account of the Amazon S3 call it could not make: Amazon
                // S3's access-denied class, naming the object read (`s3:GetObject`, the
                // action "Setting up access to an Amazon S3 bucket" grants for an import) and
                // the object of THIS statement, under the server's S3-call error number.
                let expected = format!(
                    "S3 API returned error: AccessDenied: Access Denied. The IAM role the \
                     engine loads under may not perform s3:GetObject on \
                     arn:aws:s3:::{IMPORT_BUCKET}/{}",
                    object.key
                );
                assert_eq!(
                    (refusal.code.as_str(), refusal.message.as_str()),
                    (ER_S3_API_ERROR, expected.as_str()),
                    "case {case:?}: expected the engine's own refusal of the object read, got \
                     {refusal:?}"
                );
                let loaded = session.run(&case, &read_back).await.rows;
                assert!(
                    loaded.is_empty(),
                    "case {case:?}: expected the refused import to have loaded no row, got \
                     {loaded:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The snapshot export a client polls to its end
// ---------------------------------------------------------------------------

/// Everything the export tests need: the emulator, the destination bucket the role is granted
/// on, that role, the key the objects are encrypted under, and the snapshot the export reads.
struct Fixture {
    router: MultiServiceRouter,
    bucket: String,
    role_arn: String,
    key_id: String,
    snapshot_arn: String,
}

/// The tables the exported database holds, seeded through the source instance's OWN published
/// endpoint — a client's session, so what the snapshot carries is what a client wrote.
const SEEDED: &[(&str, &[&str])] = &[
    (
        "CREATE TABLE orders (id int, customer varchar(32))",
        &[
            "INSERT INTO orders VALUES (1, 'alpha')",
            "INSERT INTO orders VALUES (2, 'beta')",
        ],
    ),
    (
        "CREATE TABLE regions (code varchar(8), name varchar(32))",
        &["INSERT INTO regions VALUES ('wa', 'Washington')"],
    ),
];

/// The tables [`SEEDED`] leaves in the database, which the export writes a file per.
const SEEDED_TABLES: [&str; 2] = ["orders", "regions"];

/// Stand the export's source and destination up, driving each through its own documented
/// lifecycle with a poll loop: the instance to the status a snapshot is taken in, and the
/// snapshot to the status an export reads.
async fn export_fixture(name: &str) -> Fixture {
    let (router, _engine) = emulator();
    let bucket = format!("emu-cpe-{name}-bucket");
    create_bucket(&router, &bucket, REGION).await;
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
        &format!("emu-cpe-{name}-role"),
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(&bucket)),
    )
    .await;

    let instance = format!("emu-cpe-{name}-source");
    expect_ok(
        name,
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => instance,
                "Engine" => "mysql",
                "DBInstanceClass" => "db.t3.micro",
                "AllocatedStorage" => 20,
                "DBName" => DATABASE,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let instance_request = params!["DBInstanceIdentifier" => instance];
    let (reported, _polls) = poll_until(
        name,
        "the source DB instance reaching the status a snapshot is taken in",
        || call(&router, "DescribeDBInstances", &instance_request),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;

    // The rows the export will write, written through the instance's OWN published endpoint.
    let (host, port) = endpoint_of(&reported);
    assert_own_address(name, &host, port).await;
    let mut session = wire_login(name, Wire::MySql, &host, port, DATABASE).await;
    for (declaration, inserts) in SEEDED {
        session.run(name, declaration).await;
        for insert in *inserts {
            session.run(name, insert).await;
        }
    }

    let snapshot = format!("emu-cpe-{name}-snapshot");
    let created = expect_ok(
        name,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    let accepted = text(&created, "Status");
    assert_eq!(
        accepted, "creating",
        "case {name:?}: expected the accepted snapshot to report creating, got {accepted:?}"
    );
    let snapshot_request = params!["DBSnapshotIdentifier" => snapshot];
    let (settled, _polls) = poll_until(
        name,
        "the DB snapshot reaching the status an export reads",
        || call(&router, "DescribeDBSnapshots", &snapshot_request),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    let snapshot_arn = text(&settled, "DBSnapshotArn");
    assert!(
        !snapshot_arn.is_empty(),
        "case {name:?}: the settled snapshot reports its ARN, got {settled}"
    );

    Fixture {
        router,
        bucket,
        role_arn,
        key_id,
        snapshot_arn,
    }
}

/// `StartExportTask` for one task of the fixture's snapshot, into `bucket`.
async fn start_export(
    fixture: &Fixture,
    identifier: &str,
    bucket: &str,
    prefix: Option<&str>,
) -> (u16, String) {
    let mut request: Vec<(&str, String)> = params![
        "ExportTaskIdentifier" => identifier,
        "SourceArn" => fixture.snapshot_arn,
        "S3BucketName" => bucket,
        "IamRoleArn" => fixture.role_arn,
        "KmsKeyId" => fixture.key_id,
    ]
    .to_vec();
    if let Some(prefix) = prefix {
        request.push(("S3Prefix", prefix.to_string()));
    }
    call(&fixture.router, "StartExportTask", &request).await
}

/// The one `<ExportTask>` subtree a report carries.
fn the_task(case: &str, body: &str) -> String {
    let reported = subtrees(body, "ExportTask");
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one reported ExportTask, got {body}"
    );
    reported.into_iter().next().expect("checked just above")
}

/// Poll `DescribeExportTasks` — a REPORTING call and nothing else — until the task has
/// stopped, and answer the `<ExportTask>` it settled on.
async fn settled_task(fixture: &Fixture, case: &str, identifier: &str) -> String {
    let request = params!["ExportTaskIdentifier" => identifier];
    let (body, _polls) = poll_until(
        case,
        "the export task reaching a terminal status",
        || call(&fixture.router, "DescribeExportTasks", &request),
        |status, body| {
            status == 200
                && subtrees(body, "ExportTask").len() == 1
                && TERMINAL_EXPORT_STATUSES
                    .contains(&text(&the_task(case, body), "Status").as_str())
        },
    )
    .await;
    the_task(case, &body)
}

/// The keys `bucket` holds under `prefix`, read through Amazon S3's own `ListObjectsV2` — so
/// what is asserted to exist is what that service really holds.
async fn listed_keys(fixture: &Fixture, bucket: &str, prefix: &str) -> Vec<String> {
    s3(&fixture.router)
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .send()
        .await
        .unwrap_or_else(|error| panic!("Amazon S3 lists {bucket} under {prefix}: {error}"))
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// Assert that the export `identifier` wrote its objects under the path its request asked
/// for, and nowhere else in `bucket`.
async fn assert_objects_under_the_task_prefix(
    fixture: &Fixture,
    case: &str,
    identifier: &str,
    prefix: Option<&str>,
) {
    let export_prefix = st::export_prefix(prefix, identifier);
    let keys = listed_keys(fixture, &fixture.bucket, &export_prefix).await;
    assert!(
        !keys.is_empty(),
        "case {case:?}: expected the completed export's objects under {export_prefix}, got none"
    );
    for key in &keys {
        assert!(
            key.starts_with(&export_prefix),
            "case {case:?}: every object of the export lies under {export_prefix}, got {key:?}"
        );
    }
    for table in SEEDED_TABLES {
        let table_prefix = format!("{export_prefix}{DATABASE}/{DATABASE}.{table}/");
        assert!(
            keys.iter()
                .any(|key| key.starts_with(&table_prefix) && key.ends_with(".gz.parquet")),
            "case {case:?}: expected the exported file of {table} under {table_prefix}, got \
             {keys:?}"
        );
    }
    assert!(
        keys.contains(&st::export_info_key(&export_prefix, identifier)),
        "case {case:?}: expected the export's own summary file at {}, got {keys:?}",
        st::export_info_key(&export_prefix, identifier)
    );
}

/// One row of the export table: the task's identifier and the prefix its request names.
struct ExportRow {
    case: &'static str,
    identifier: &'static str,
    prefix: Option<&'static str>,
}

const EXPORT_ROWS: &[ExportRow] = &[
    ExportRow {
        case: "an export at the root of the bucket",
        identifier: "emu-cpe-export-all",
        prefix: None,
    },
    ExportRow {
        case: "an export under a prefix",
        identifier: "emu-cpe-export-prefixed",
        prefix: Some("nightly/exports"),
    },
];

/// RULE: a client that only reports reaches the end of the snapshot-export lifecycle — the
/// snapshot settles `available` and the task settles `COMPLETE`, with the progress and the end
/// time of that status — and the objects the export wrote lie under
/// `<S3Prefix>/<ExportTaskIdentifier>/` in the destination bucket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_snapshot_export_lifecycle_settles_and_writes_its_objects_under_the_task_prefix() {
    let fixture = export_fixture("settles").await;

    for row in EXPORT_ROWS {
        let started = expect_ok(
            row.case,
            start_export(&fixture, row.identifier, &fixture.bucket, row.prefix).await,
        );
        let accepted = text(&started, "Status");
        assert_eq!(
            accepted, "STARTING",
            "case {:?}: expected the accepted export to report STARTING, got {accepted:?}",
            row.case
        );
        let not_ended = optional_text(&started, "TaskEndTime");
        assert!(
            not_ended.is_none(),
            "case {:?}: a task that has not stopped reports no TaskEndTime, got {not_ended:?}",
            row.case
        );

        // Only the clock moves it: this loop makes reporting calls and nothing else.
        let settled = settled_task(&fixture, row.case, row.identifier).await;
        let status = text(&settled, "Status");
        assert_eq!(
            status, "COMPLETE",
            "case {:?}: expected an export nothing cancelled to finish COMPLETE, got {settled}",
            row.case
        );
        let progress = text(&settled, "PercentProgress");
        assert_eq!(
            progress,
            st::COMPLETE_PERCENT_PROGRESS.to_string(),
            "case {:?}: expected a completed export to report all of its source written, got \
             {progress:?}",
            row.case
        );
        let ended = optional_text(&settled, "TaskEndTime");
        assert!(
            ended.is_some(),
            "case {:?}: a task that has stopped reports when it stopped, got {settled}",
            row.case
        );

        assert_objects_under_the_task_prefix(&fixture, row.case, row.identifier, row.prefix)
            .await;
    }
}

/// The buckets the fixture's role is NOT granted on: two of them, so the bucket in the
/// refusal's sentence varies across the table.
const UNGRANTED_BUCKETS: [&str; 2] = ["emu-cpe-refused-one", "emu-cpe-refused-two"];

/// RULE: an export whose role is not authorized on the bucket the request names is refused
/// before anything is written — HTTP 400 `IamRoleMissingPermissions`, carrying the sentence
/// Amazon RDS words that class as with the request's role, the first call the role cannot make
/// and the bucket in it — and the SAME role is accepted against the bucket it is granted on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_export_to_an_ungranted_bucket_is_refused_and_writes_nothing() {
    let fixture = export_fixture("refused").await;
    for bucket in UNGRANTED_BUCKETS {
        create_bucket(&fixture.router, bucket, REGION).await;
    }

    for (index, bucket) in UNGRANTED_BUCKETS.into_iter().enumerate() {
        let case = format!("a bucket the role is not granted on ({bucket})");
        let identifier = format!("emu-cpe-refused-{index}");
        let body = expect_error(
            &case,
            start_export(&fixture, &identifier, bucket, None).await,
            400,
            "IamRoleMissingPermissions",
        );
        // The export's FIRST call on its destination is the bucket-location read, so that is
        // the call a role granted nothing there cannot make.
        let expected = format!(
            "The IAM role {} isn't authorized to call s3:GetBucketLocation on the S3 bucket \
             {bucket}.",
            fixture.role_arn
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal message {expected:?}, got {message:?}"
        );

        // Nothing was written, and no task stands for the refused export.
        let keys = listed_keys(&fixture, bucket, &st::export_prefix(None, &identifier)).await;
        assert!(
            keys.is_empty(),
            "case {case:?}: a refused export writes no object, got {keys:?}"
        );
        let reported = expect_ok(
            &case,
            call(
                &fixture.router,
                "DescribeExportTasks",
                &params!["ExportTaskIdentifier" => identifier],
            )
            .await,
        );
        assert!(
            subtrees(&reported, "ExportTask").is_empty(),
            "case {case:?}: a refused export records no task, got {reported}"
        );
    }

    // And the same role, against the bucket it IS granted on, is accepted and runs to the end.
    let case = "the bucket the same role is granted on";
    let identifier = "emu-cpe-refused-then-granted";
    let started = expect_ok(
        case,
        start_export(&fixture, identifier, &fixture.bucket, None).await,
    );
    let accepted = text(&started, "Status");
    assert_eq!(
        accepted, "STARTING",
        "case {case:?}: expected the accepted export to report STARTING, got {accepted:?}"
    );
    let settled = settled_task(&fixture, case, identifier).await;
    let status = text(&settled, "Status");
    assert_eq!(
        status, "COMPLETE",
        "case {case:?}: expected the accepted export to finish COMPLETE, got {settled}"
    );
    assert_objects_under_the_task_prefix(&fixture, case, identifier, None).await;
}
