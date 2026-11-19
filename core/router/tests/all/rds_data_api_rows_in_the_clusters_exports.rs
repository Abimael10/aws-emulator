// SPDX-License-Identifier: CC0-1.0

//! A DB cluster's data is what its clients WROTE, through whichever of its endpoints they
//! wrote it — so the rows written through its Data API HTTP endpoint are in the copy a cluster
//! snapshot carries and in the copy an export reads.
//!
//! RULE: a copy of a DB cluster's data holds the tables a client created through ANY endpoint
//! of that cluster, so an export of that cluster — or of a snapshot of it — writes one Parquet
//! object per such table under `<S3Prefix>/<ExportTaskIdentifier>/`, with the rows that table
//! really holds.
//!
//! Only a cross-service suite can pin this: the cluster is Amazon RDS's resource, its Data API
//! HTTP endpoint is served by the RDS Data API service, the credential that call reads is
//! Secrets Manager's, the export's destination and key belong to Amazon S3 and KMS and its
//! role to IAM, and the exported objects are read back through Amazon S3's own reads — seven
//! services over ONE engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! The cases below span what the rule generalises over: the database a Data API call NAMES and
//! the cluster's own default database that a call naming none writes into, a table of one row
//! and a table of several, a table whose columns carry each kind of value a row can hold (an
//! integer, text, a fractional number and a missing value), and both export sources — the
//! cluster snapshot and the cluster itself.
//!
//! Every call goes over the wire, on a router with request authentication off as every suite
//! in this crate runs it, and every wait is a poll for the CONDITION under a generous
//! deadline. Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, element, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-west-2";

/// The destination bucket both exports write into.
const BUCKET: &str = "rds-data-api-export-destination";

/// The cluster this suite provisions, the database it is created with, and the member instance
/// that makes it serve.
const CLUSTER: &str = "data-api-export-cluster";
/// See [`CLUSTER`].
const DATABASE: &str = "appdata";
/// See [`CLUSTER`].
const INSTANCE: &str = "data-api-export-instance";
/// The cluster snapshot one of the two exports reads.
const SNAPSHOT: &str = "data-api-export-snapshot";

/// The master user the cluster is created with, and the credential the Data API reads it as.
const MASTER_USER: &str = "appadmin";
/// See [`MASTER_USER`].
const MASTER_PASSWORD: &str = "app-Secret-99";

/// How long a poll waits for a state the RDS control plane owes before giving up: generous, so
/// a loaded host does not fail a condition that does arrive, and far short of a hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads. The condition, not the clock, is what ends the
/// loop; this only keeps it from spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The trust policy the export role carries: the document that lets Amazon RDS's export
/// service principal assume it.
const EXPORT_TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"export.rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The policy the export role carries, as the user guide's export role does: the object writes
/// and the bucket read an export of a snapshot to Amazon S3 performs.
const EXPORT_ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["s3:PutObject*","s3:GetObject*","s3:ListBucket","s3:DeleteObject*","#,
    r#""s3:GetBucketLocation"],"Resource":["#,
    r#""arn:aws:s3:::rds-data-api-export-destination","#,
    r#""arn:aws:s3:::rds-data-api-export-destination/*"]}]}"#
);

/// One table seeded through the cluster's Data API HTTP endpoint, and what the export of it
/// must then hold.
struct Seed {
    /// What this row is a case of.
    case: &'static str,
    /// The `database` member the seeding calls carry, or `None` for a call that names none —
    /// which writes into the database the cluster was created with.
    database: Option<&'static str>,
    /// The table the seed creates.
    table: &'static str,
    /// The statements that create it and fill it, in order.
    statements: &'static [&'static str],
    /// How many rows it then holds, which is what the export's own file reports for it.
    rows: u64,
}

/// The seeds: a table of the database the call names and a table of the cluster's default
/// database named by no call, one row and several, and a table carrying each kind of value a
/// row can hold.
const SEEDS: &[Seed] = &[
    Seed {
        case: "a table of the database the calls name",
        database: Some(DATABASE),
        table: "named_rows",
        statements: &[
            "CREATE TABLE named_rows (id INT PRIMARY KEY, label VARCHAR(40))",
            "INSERT INTO named_rows (id, label) VALUES (1, 'alpha')",
            "INSERT INTO named_rows (id, label) VALUES (2, 'beta')",
            "INSERT INTO named_rows (id, label) VALUES (3, 'gamma')",
        ],
        rows: 3,
    },
    Seed {
        case: "a one-row table of the cluster's default database, named by no call",
        database: None,
        table: "default_rows",
        statements: &[
            "CREATE TABLE default_rows (id INT PRIMARY KEY, label VARCHAR(40))",
            "INSERT INTO default_rows (id, label) VALUES (1, 'only')",
        ],
        rows: 1,
    },
    Seed {
        case: "a table carrying each kind of value a row holds",
        database: Some(DATABASE),
        table: "typed_rows",
        statements: &[
            "CREATE TABLE typed_rows (id INT PRIMARY KEY, label VARCHAR(40), \
             amount DOUBLE, absent VARCHAR(10))",
            "INSERT INTO typed_rows (id, label, amount, absent) \
             VALUES (1, 'first', 12.5, NULL)",
            "INSERT INTO typed_rows (id, label, amount, absent) \
             VALUES (2, 'second', -0.25, 'here')",
        ],
        rows: 2,
    },
];

/// One export this suite runs: which source it reads and where it writes.
struct Export {
    /// What this row is a case of.
    case: &'static str,
    /// The export task's identifier, which is also the folder its objects sit in.
    identifier: &'static str,
    /// The `S3Prefix` the objects are written under.
    prefix: &'static str,
    /// Whether `SourceArn` is the cluster SNAPSHOT's ARN (else the cluster's own).
    from_the_snapshot: bool,
}

/// Both sources an export of this cluster's data can name.
const EXPORTS: &[Export] = &[
    Export {
        case: "an export of the cluster snapshot",
        identifier: "data-api-snapshot-export",
        prefix: "from-the-snapshot",
        from_the_snapshot: true,
    },
    Export {
        case: "an export of the cluster itself",
        identifier: "data-api-cluster-export",
        prefix: "from-the-cluster",
        from_the_snapshot: false,
    },
];

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// Assert the call was served, and hand back its body.
    fn served(self, case: &str) -> String {
        assert_eq!(
            self.status, 200,
            "case {case:?}: the call must be served, got HTTP {} with {}",
            self.status, self.body
        );
        self.body
    }
}

/// One call of `method` to the service `signing_name` at `path`, with `body`.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: String,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            // The signature is never checked: this router runs with authentication off.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One awsQuery call to the service `signing_name`, as an SDK client sends one.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        runtime,
        signing_name,
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
}

/// One awsQuery call to Amazon RDS.
fn rds(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    query(router, runtime, "rds", "2014-10-31", action, params)
}

/// Read `read` until `settled` holds of what it answered, and hand that answer back.
///
/// The CONDITION ends the loop, never the clock: the deadline is only there so a state that
/// never arrives fails with what was last seen instead of hanging.
fn poll_until(
    case: &str,
    mut read: impl FnMut() -> String,
    settled: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut last = read();
    while !settled(&last) {
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the state polled for never arrived; last answer was {last}"
        );
        std::thread::sleep(POLL_INTERVAL);
        last = read();
    }
    last
}

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The rows a Data API client wrote are in the cluster's snapshot and in both exports of its
/// data, as the Parquet objects of the tables that client created.
#[test]
fn data_api_rows_are_exported_with_the_clusters_data() {
    std::thread::Builder::new()
        .name("rds-data-api-export".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a DB instance
            // create opens the instance's engine listener through the core, and binding a
            // socket needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // ---- the destination: a bucket, a key and a role ----
            dispatch(
                &router,
                &runtime,
                "s3",
                "PUT",
                &format!("/{BUCKET}"),
                &[],
                format!(
                    "<CreateBucketConfiguration><LocationConstraint>{REGION}\
                     </LocationConstraint></CreateBucketConfiguration>"
                ),
            )
            .served("create the destination bucket");
            let key = dispatch(
                &router,
                &runtime,
                "kms",
                "POST",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", "TrentService.CreateKey"),
                ],
                "{}".to_string(),
            )
            .served("create the export's KMS key");
            let key: Value = serde_json::from_str(&key).expect("KMS answers JSON");
            let kms_key_id = key["KeyMetadata"]["KeyId"]
                .as_str()
                .unwrap_or_else(|| panic!("KMS reports the key it minted: {key}"))
                .to_string();
            let role = query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "CreateRole",
                &params![
                    "RoleName" => "data-api-export-role",
                    "AssumeRolePolicyDocument" => EXPORT_TRUST_POLICY,
                ],
            )
            .served("create the export role");
            let role_arn = element(&role, "Arn");
            query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "PutRolePolicy",
                &params![
                    "RoleName" => "data-api-export-role",
                    "PolicyName" => "data-api-export-to-s3",
                    "PolicyDocument" => EXPORT_ROLE_POLICY,
                ],
            )
            .served("attach the export role's policy");

            // ---- the network the DB subnet group is over ----
            let vpc = query(
                &router,
                &runtime,
                "ec2",
                "2016-11-15",
                "CreateVpc",
                &params!["CidrBlock" => "10.43.0.0/16"],
            )
            .served("create the VPC");
            let vpc_id = element(&vpc, "vpcId");
            let mut subnets = Vec::new();
            for (index, zone) in ["us-west-2a", "us-west-2b"].iter().enumerate() {
                let subnet = query(
                    &router,
                    &runtime,
                    "ec2",
                    "2016-11-15",
                    "CreateSubnet",
                    &params![
                        "VpcId" => vpc_id,
                        "CidrBlock" => format!("10.43.{index}.0/24"),
                        "AvailabilityZone" => zone,
                    ],
                )
                .served("create a subnet");
                subnets.push(element(&subnet, "subnetId"));
            }

            // ---- the source: a subnet group, an Aurora cluster and a member instance ----
            rds(
                &router,
                &runtime,
                "CreateDBSubnetGroup",
                &params![
                    "DBSubnetGroupName" => "data-api-export-subnets",
                    "DBSubnetGroupDescription" => "the export suite's subnets",
                    "SubnetIds.SubnetIdentifier.1" => subnets[0],
                    "SubnetIds.SubnetIdentifier.2" => subnets[1],
                ],
            )
            .served("create the DB subnet group");
            let cluster = rds(
                &router,
                &runtime,
                "CreateDBCluster",
                &params![
                    "DBClusterIdentifier" => CLUSTER,
                    "Engine" => "aurora-mysql",
                    "MasterUsername" => MASTER_USER,
                    "MasterUserPassword" => MASTER_PASSWORD,
                    "DatabaseName" => DATABASE,
                    "EnableHttpEndpoint" => "true",
                    "ServerlessV2ScalingConfiguration.MinCapacity" => "0.5",
                    "ServerlessV2ScalingConfiguration.MaxCapacity" => "2.0",
                    "StorageEncrypted" => "true",
                    "BackupRetentionPeriod" => "1",
                    "DBSubnetGroupName" => "data-api-export-subnets",
                ],
            )
            .served("create the DB cluster");
            let cluster_arn = element(&cluster, "DBClusterArn");
            rds(
                &router,
                &runtime,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => INSTANCE,
                    "Engine" => "aurora-mysql",
                    "DBInstanceClass" => "db.serverless",
                    "DBClusterIdentifier" => CLUSTER,
                ],
            )
            .served("create the member DB instance");
            poll_until(
                "the member instance becomes available",
                || {
                    rds(
                        &router,
                        &runtime,
                        "DescribeDBInstances",
                        &params!["DBInstanceIdentifier" => INSTANCE],
                    )
                    .served("describe the member DB instance")
                },
                |answer| element(answer, "DBInstanceStatus") == "available",
            );

            // ---- the data: written through the cluster's Data API HTTP endpoint ----
            let secret = dispatch(
                &router,
                &runtime,
                "secretsmanager",
                "POST",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", "secretsmanager.CreateSecret"),
                ],
                serde_json::json!({
                    "Name": "data-api-export",
                    // An SDK generates this idempotency token itself; a raw request carries
                    // its own.
                    "ClientRequestToken": "4c2e9a71-0b35-4d8f-9a62-1f7c3d5e8b04",
                    "SecretString":
                        format!("{{\"username\":\"{MASTER_USER}\",\"password\":\"{MASTER_PASSWORD}\"}}"),
                })
                .to_string(),
            )
            .served("create the cluster's credential secret");
            let secret: Value =
                serde_json::from_str(&secret).expect("Secrets Manager answers JSON");
            let secret_arn = secret["ARN"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("Secrets Manager reports the secret it minted: {secret}")
                })
                .to_string();
            for seed in SEEDS {
                for statement in seed.statements {
                    let mut call = serde_json::json!({
                        "resourceArn": cluster_arn,
                        "secretArn": secret_arn,
                        "sql": statement,
                    });
                    if let Some(database) = seed.database {
                        call["database"] = Value::String(database.to_string());
                    }
                    dispatch(
                        &router,
                        &runtime,
                        "rds-data",
                        "POST",
                        "/Execute",
                        &[("content-type", "application/json")],
                        call.to_string(),
                    )
                    .served(seed.case);
                }
            }

            // ---- the snapshot one of the two exports reads ----
            rds(
                &router,
                &runtime,
                "CreateDBClusterSnapshot",
                &params![
                    "DBClusterSnapshotIdentifier" => SNAPSHOT,
                    "DBClusterIdentifier" => CLUSTER,
                ],
            )
            .served("snapshot the DB cluster");
            let snapshot = poll_until(
                "the cluster snapshot becomes available",
                || {
                    rds(
                        &router,
                        &runtime,
                        "DescribeDBClusterSnapshots",
                        &params!["DBClusterSnapshotIdentifier" => SNAPSHOT],
                    )
                    .served("describe the DB cluster snapshot")
                },
                |answer| element(answer, "Status") == "available",
            );
            let snapshot_arn = element(&snapshot, "DBClusterSnapshotArn");

            // ---- and each export of that data ----
            for export in EXPORTS {
                let source_arn = if export.from_the_snapshot {
                    snapshot_arn.clone()
                } else {
                    cluster_arn.clone()
                };
                rds(
                    &router,
                    &runtime,
                    "StartExportTask",
                    &params![
                        "ExportTaskIdentifier" => export.identifier,
                        "SourceArn" => source_arn,
                        "S3BucketName" => BUCKET,
                        "S3Prefix" => export.prefix,
                        "IamRoleArn" => role_arn,
                        "KmsKeyId" => kms_key_id,
                    ],
                )
                .served(export.case);
                let described = poll_until(
                    export.case,
                    || {
                        rds(
                            &router,
                            &runtime,
                            "DescribeExportTasks",
                            &params!["ExportTaskIdentifier" => export.identifier],
                        )
                        .served(export.case)
                    },
                    |answer| {
                        ["COMPLETE", "FAILED", "CANCELED"]
                            .contains(&element(answer, "Status").as_str())
                    },
                );
                let settled = (
                    element(&described, "Status"),
                    element(&described, "PercentProgress"),
                    opt_element(&described, "FailureCause"),
                );
                assert_eq!(
                    settled,
                    ("COMPLETE".to_string(), "100".to_string(), None),
                    "case {:?}: an export that wrote its objects is COMPLETE at 100% with no \
                     failure cause, got {described}",
                    export.case
                );

                // What the destination bucket now holds under this export's own folder, read
                // back through Amazon S3's own listing.
                let under = format!("{}/{}/", export.prefix, export.identifier);
                let listed = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!(
                        "/{BUCKET}?list-type=2&prefix={}",
                        form_encode(&under)
                    ),
                    &[],
                    String::new(),
                )
                .served("list the export's own folder");
                let objects: Vec<(String, u64)> = blocks_of(&listed, "Contents")
                    .into_iter()
                    .map(|entry| {
                        (
                            element(entry, "Key"),
                            element(entry, "Size").parse::<u64>().unwrap_or_default(),
                        )
                    })
                    .collect();

                // One Parquet object per seeded table, under the database and table it came
                // from, and not empty: the rows are IN it.
                for seed in SEEDS {
                    let table_prefix =
                        format!("{under}{DATABASE}/{DATABASE}.{}/part-00000-", seed.table);
                    let written: Vec<&(String, u64)> = objects
                        .iter()
                        .filter(|(key, _)| {
                            key.starts_with(&table_prefix) && key.ends_with(".gz.parquet")
                        })
                        .collect();
                    assert_eq!(
                        written.len(),
                        1,
                        "case {:?} / {:?}: expected one Parquet object under {table_prefix:?}, \
                         got {objects:?}",
                        export.case,
                        seed.case
                    );
                    assert!(
                        written[0].1 > 0,
                        "case {:?} / {:?}: the Parquet object {:?} carries the table's rows, so \
                         it is not empty, got {} bytes",
                        export.case,
                        seed.case,
                        written[0].0,
                        written[0].1
                    );
                }

                // And the export's own file says what it exported: every seeded table, with
                // the rows that table really holds.
                let (manifest, _) = objects
                    .iter()
                    .find(|(key, _)| {
                        key.starts_with(&format!("{under}export_tables_info_"))
                            && key.ends_with(".json")
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: the export writes the file that says what it exported, \
                             got {objects:?}",
                            export.case
                        )
                    });
                let document = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!("/{BUCKET}/{manifest}"),
                    &[],
                    String::new(),
                )
                .served("read the export's own file");
                let document: Value =
                    serde_json::from_str(&document).expect("the export's file is JSON");
                let reported: Vec<(String, u64)> = document["perTableStatus"]
                    .as_array()
                    .unwrap_or_else(|| panic!("the file lists what it exported: {document}"))
                    .iter()
                    .map(|entry| {
                        (
                            entry["table"].as_str().unwrap_or_default().to_string(),
                            entry["tableStatistics"]["rowsExported"]
                                .as_u64()
                                .unwrap_or_default(),
                        )
                    })
                    .collect();
                for seed in SEEDS {
                    let rows = reported
                        .iter()
                        .find(|(table, _)| table == seed.table)
                        .map(|(_, rows)| *rows);
                    assert_eq!(
                        rows,
                        Some(seed.rows),
                        "case {:?} / {:?}: the export reports the rows the table holds, got \
                         {reported:?}",
                        export.case,
                        seed.case
                    );
                }
            }

            // The cluster's own data plane is untouched by either export: the tables are still
            // there to be read through the Data API, which is what the exports copied out of.
            for seed in SEEDS {
                let mut call = serde_json::json!({
                    "resourceArn": cluster_arn,
                    "secretArn": secret_arn,
                    "sql": format!("SELECT count(*) AS n FROM {}", seed.table),
                });
                if let Some(database) = seed.database {
                    call["database"] = Value::String(database.to_string());
                }
                let counted = dispatch(
                    &router,
                    &runtime,
                    "rds-data",
                    "POST",
                    "/Execute",
                    &[("content-type", "application/json")],
                    call.to_string(),
                )
                .served(seed.case);
                let counted: Value = serde_json::from_str(&counted).expect("rds-data answers JSON");
                let rows = counted["records"][0][0]["longValue"].as_u64();
                assert_eq!(
                    rows,
                    Some(seed.rows),
                    "case {:?}: the cluster still holds the rows the exports read, got {counted}",
                    seed.case
                );
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to its assertions");
}
