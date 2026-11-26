// SPDX-License-Identifier: CC0-1.0

//! The MANIFEST an Amazon RDS export to Amazon S3 leaves behind, read back out of the
//! destination bucket after the whole Aurora-seeded chain has run.
//!
//! RULE: a completed export writes, under the folder its own `ExportTaskIdentifier` names
//! below the `S3Prefix` the request gave — and at the bucket root for a request that gave
//! none — the two JSON documents that describe it: the export's summary
//! (`export_info_<id>.json`) and the per-table file (`export_tables_info_<id>_from_1_to_<n>.json`).
//! Each is an object a client reads with Amazon S3's own `GetObject`, and each describes the
//! export that RAN: its identifier, the source it read and that source's type, the destination
//! bucket and the prefix as the request named them, the key the data was encrypted under, the
//! `ExportOnly` it was limited to, and one entry per table it really wrote carrying that
//! table's row count and naming the object its rows went into.
//!
//! Every milestone of the chain the manifest sits at the end of has to ARRIVE for a client to
//! read it at all: the `db.serverless` member instance reports `available`, the Data API
//! statements against the cluster's HTTP endpoint are served, the DB cluster snapshot reports
//! `available`, and the export reaches a terminal status. A client that polls for any one of
//! those and never sees it has nothing to branch on, so each is polled for here and asserted.
//!
//! Only a cross-service suite can pin this: the cluster and the export task are Amazon RDS's,
//! the HTTP endpoint the rows are written through is the RDS Data API's, the credential that
//! call reads is Secrets Manager's, the network the subnet group spans is EC2's, the
//! destination bucket and the manifest reads are Amazon S3's, and the export's key and role
//! are KMS's and IAM's — seven services over ONE engine, exactly as `core/router/src/lib.rs`
//! mounts them.
//!
//! The cases below span what the rule generalises over: a multi-segment prefix, a
//! single-segment one and no prefix at all; an `ExportOnly` naming nothing, one
//! `database.table` and one whole database; and both sources an export of this data can name,
//! the cluster snapshot and the cluster itself.
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

/// The destination bucket every export of this suite writes into.
const BUCKET: &str = "rds-export-manifest-destination";

/// The cluster this suite provisions, the database it is created with, and the member
/// instance that makes it serve.
const CLUSTER: &str = "manifest-export-cluster";
/// See [`CLUSTER`].
const DATABASE: &str = "appdata";
/// See [`CLUSTER`].
const INSTANCE: &str = "manifest-export-instance";
/// The cluster snapshot three of the four exports read.
const SNAPSHOT: &str = "manifest-export-snapshot";

/// The master user the cluster is created with, and the credential the Data API reads it as.
const MASTER_USER: &str = "appadmin";
/// See [`MASTER_USER`].
const MASTER_PASSWORD: &str = "app-Secret-99";

/// The IAM role the exports run as.
const ROLE: &str = "manifest-export-role";

/// How long a poll waits for a state the control plane owes before giving up: generous, so a
/// loaded host does not fail a condition that does arrive, and far short of a hang.
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

/// The policy the export role carries, as the user guide's export role does: the object
/// writes and the bucket read an export to Amazon S3 performs.
const EXPORT_ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["s3:PutObject*","s3:GetObject*","s3:ListBucket","s3:DeleteObject*","#,
    r#""s3:GetBucketLocation"],"Resource":["#,
    r#""arn:aws:s3:::rds-export-manifest-destination","#,
    r#""arn:aws:s3:::rds-export-manifest-destination/*"]}]}"#
);

/// One table seeded into the cluster's database through its Data API HTTP endpoint, and how
/// many rows it then holds — which is what every manifest of an export covering it reports.
struct Seed {
    /// The table the seed creates.
    table: &'static str,
    /// The statements that create it and fill it, in order. They are written the way a client
    /// writes an idempotent seeding step, so a client that retries one stores the same rows.
    statements: &'static [&'static str],
    /// How many rows it then holds.
    rows: u64,
}

/// The two tables the cluster's database holds once the seeding step has run.
const SEEDS: &[Seed] = &[
    Seed {
        table: "export_rows",
        statements: &[
            "CREATE TABLE IF NOT EXISTS export_rows (id INT PRIMARY KEY, val VARCHAR(50))",
            "INSERT INTO export_rows (id, val) VALUES (1, 'alpha') \
             ON DUPLICATE KEY UPDATE val = VALUES(val)",
            "INSERT INTO export_rows (id, val) VALUES (2, 'beta') \
             ON DUPLICATE KEY UPDATE val = VALUES(val)",
        ],
        rows: 2,
    },
    Seed {
        table: "export_tags",
        statements: &[
            "CREATE TABLE IF NOT EXISTS export_tags (id INT PRIMARY KEY, tag VARCHAR(20))",
            "INSERT INTO export_tags (id, tag) VALUES (1, 'red') \
             ON DUPLICATE KEY UPDATE tag = VALUES(tag)",
            "INSERT INTO export_tags (id, tag) VALUES (2, 'green') \
             ON DUPLICATE KEY UPDATE tag = VALUES(tag)",
            "INSERT INTO export_tags (id, tag) VALUES (3, 'blue') \
             ON DUPLICATE KEY UPDATE tag = VALUES(tag)",
        ],
        rows: 3,
    },
];

/// One export this suite runs, and what its manifest must then say.
struct ExportCase {
    /// What this row is a case of.
    case: &'static str,
    /// The export task's identifier, which is also the folder its objects sit in.
    identifier: &'static str,
    /// The `S3Prefix` the request carries, or `None` for a request that carries none.
    prefix: Option<&'static str>,
    /// The `ExportOnly` entries the request carries; empty for an export of the whole source.
    export_only: &'static [&'static str],
    /// Whether `SourceArn` is the cluster SNAPSHOT's ARN (else the cluster's own).
    from_the_snapshot: bool,
    /// The tables of the cluster's own database the manifest must report, by name.
    tables: &'static [&'static str],
    /// Whether the manifest may report a table of ANY OTHER database: false for an export an
    /// `ExportOnly` limited to this one, true for an export of the whole source, whose
    /// storage carries the server's own schema beside the client's database.
    other_databases: bool,
}

/// The exports: a multi-segment prefix, a single-segment one and none; an `ExportOnly` of
/// zero, one and two segments; and both sources an export of this data can name.
const EXPORTS: &[ExportCase] = &[
    ExportCase {
        case: "the whole snapshot, under a multi-segment prefix",
        identifier: "manifest-whole-nested",
        prefix: Some("team/exports"),
        export_only: &[],
        from_the_snapshot: true,
        tables: &["export_rows", "export_tags"],
        other_databases: true,
    },
    ExportCase {
        case: "the whole snapshot, written at the bucket root",
        identifier: "manifest-whole-root",
        prefix: None,
        export_only: &[],
        from_the_snapshot: true,
        tables: &["export_rows", "export_tags"],
        other_databases: true,
    },
    ExportCase {
        case: "the snapshot, limited to one database.table",
        identifier: "manifest-one-table",
        prefix: Some("narrow"),
        export_only: &["appdata.export_rows"],
        from_the_snapshot: true,
        tables: &["export_rows"],
        other_databases: false,
    },
    ExportCase {
        case: "the cluster itself, limited to its database",
        identifier: "manifest-cluster-database",
        prefix: Some("from-the-cluster"),
        export_only: &["appdata"],
        from_the_snapshot: false,
        tables: &["export_rows", "export_tags"],
        other_databases: false,
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
    params: &[(String, String)],
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
    params: &[(String, String)],
) -> Answer {
    query(router, runtime, "rds", "2014-10-31", action, params)
}

/// One request parameter.
fn p(name: &str, value: impl std::fmt::Display) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// Read `read` until `settled` holds of what it answered, and hand that answer back.
///
/// The CONDITION ends the loop, never the clock: the deadline is only there so a state that
/// never arrives fails with what was last seen instead of hanging — which is the whole point
/// of this suite, since a milestone that never arrives is what leaves a client waiting.
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

/// The `(key, size)` of every object the destination bucket holds under `prefix`, read back
/// through Amazon S3's own listing.
fn listed_objects(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    prefix: &str,
) -> Vec<(String, u64)> {
    let listed = dispatch(
        router,
        runtime,
        "s3",
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={}", form_encode(prefix)),
        &[],
        String::new(),
    )
    .served("list the export's own folder");
    blocks_of(&listed, "Contents")
        .into_iter()
        .map(|entry| {
            (
                element(entry, "Key"),
                element(entry, "Size").parse::<u64>().unwrap_or_default(),
            )
        })
        .collect()
}

/// One object of the destination bucket, read back through Amazon S3's own `GetObject` and
/// parsed as the JSON document a manifest is.
fn manifest_document(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    key: &str,
) -> Value {
    let body = dispatch(
        router,
        runtime,
        "s3",
        "GET",
        &format!("/{BUCKET}/{key}"),
        &[],
        String::new(),
    )
    .served(&format!("case {case:?}: read {key}"));
    serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("case {case:?}: {key} is a JSON object: {error} in {body}"))
}

/// The `(database, table, rows, files)` of every entry of a manifest's table list.
fn reported_tables(document: &Value, member: &str) -> Vec<(String, String, u64, Vec<String>)> {
    document[member]
        .as_array()
        .unwrap_or_else(|| panic!("the manifest's {member} is the list of what it covered: {document}"))
        .iter()
        .map(|entry| {
            let rows = entry["rowsExported"]
                .as_u64()
                .or_else(|| entry["tableStatistics"]["rowsExported"].as_u64())
                .unwrap_or_default();
            let files = entry["files"]
                .as_array()
                .map(|files| {
                    files
                        .iter()
                        .map(|file| file.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            (
                entry["database"].as_str().unwrap_or_default().to_string(),
                entry["table"].as_str().unwrap_or_default().to_string(),
                rows,
                files,
            )
        })
        .collect()
}

/// Every milestone of the Aurora-seeded export chain arrives, and the manifest the export
/// leaves in Amazon S3 describes the export that ran.
#[test]
fn the_seeded_exports_manifest_is_readable_in_s3() {
    std::thread::Builder::new()
        .name("rds-export-manifest".to_string())
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
                &[
                    p("RoleName", ROLE),
                    p("AssumeRolePolicyDocument", EXPORT_TRUST_POLICY),
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
                &[
                    p("RoleName", ROLE),
                    p("PolicyName", "manifest-export-to-s3"),
                    p("PolicyDocument", EXPORT_ROLE_POLICY),
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
                &[p("CidrBlock", "10.47.0.0/16")],
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
                    &[
                        p("VpcId", &vpc_id),
                        p("CidrBlock", format!("10.47.{index}.0/24")),
                        p("AvailabilityZone", zone),
                    ],
                )
                .served("create a subnet");
                subnets.push(element(&subnet, "subnetId"));
            }

            // ---- the source: a subnet group, an Aurora Serverless v2 cluster whose HTTP
            // endpoint is enabled, and the db.serverless member instance that serves it ----
            rds(
                &router,
                &runtime,
                "CreateDBSubnetGroup",
                &[
                    p("DBSubnetGroupName", "manifest-export-subnets"),
                    p("DBSubnetGroupDescription", "the manifest suite's subnets"),
                    p("SubnetIds.SubnetIdentifier.1", &subnets[0]),
                    p("SubnetIds.SubnetIdentifier.2", &subnets[1]),
                ],
            )
            .served("create the DB subnet group");
            let cluster = rds(
                &router,
                &runtime,
                "CreateDBCluster",
                &[
                    p("DBClusterIdentifier", CLUSTER),
                    p("Engine", "aurora-mysql"),
                    p("MasterUsername", MASTER_USER),
                    p("MasterUserPassword", MASTER_PASSWORD),
                    p("DatabaseName", DATABASE),
                    p("EnableHttpEndpoint", "true"),
                    p("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
                    p("ServerlessV2ScalingConfiguration.MaxCapacity", "2.0"),
                    p("StorageEncrypted", "true"),
                    p("BackupRetentionPeriod", "1"),
                    p("DBSubnetGroupName", "manifest-export-subnets"),
                ],
            )
            .served("create the DB cluster");
            let cluster_arn = element(&cluster, "DBClusterArn");
            rds(
                &router,
                &runtime,
                "CreateDBInstance",
                &[
                    p("DBInstanceIdentifier", INSTANCE),
                    p("Engine", "aurora-mysql"),
                    p("DBInstanceClass", "db.serverless"),
                    p("DBClusterIdentifier", CLUSTER),
                ],
            )
            .served("create the member DB instance");
            // MILESTONE: the instance reports the status a client waits for before it writes.
            let instance = poll_until(
                "the member instance becomes available",
                || {
                    rds(
                        &router,
                        &runtime,
                        "DescribeDBInstances",
                        &[p("DBInstanceIdentifier", INSTANCE)],
                    )
                    .served("describe the member DB instance")
                },
                |answer| element(answer, "DBInstanceStatus") == "available",
            );
            assert_eq!(
                element(&instance, "DBInstanceStatus"),
                "available",
                "the member instance must reach available, got {instance}"
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
                    "Name": "manifest-export-credentials",
                    // An SDK generates this idempotency token itself; a raw request carries
                    // its own.
                    "ClientRequestToken": "0f1c6b2e-5a74-4f20-9d31-8e6b2a4c7d15",
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
            // MILESTONE: each seeding statement is SERVED, which is what the chain's retrying
            // seed step waits for.
            for seed in SEEDS {
                for statement in seed.statements {
                    dispatch(
                        &router,
                        &runtime,
                        "rds-data",
                        "POST",
                        "/Execute",
                        &[("content-type", "application/json")],
                        serde_json::json!({
                            "resourceArn": cluster_arn,
                            "secretArn": secret_arn,
                            "database": DATABASE,
                            "sql": statement,
                        })
                        .to_string(),
                    )
                    .served(&format!("seed {} through the Data API", seed.table));
                }
            }

            // ---- the snapshot the snapshot-sourced exports read ----
            rds(
                &router,
                &runtime,
                "CreateDBClusterSnapshot",
                &[
                    p("DBClusterSnapshotIdentifier", SNAPSHOT),
                    p("DBClusterIdentifier", CLUSTER),
                ],
            )
            .served("snapshot the DB cluster");
            // MILESTONE: the snapshot reports the status an export's source has to be in.
            let snapshot = poll_until(
                "the cluster snapshot becomes available",
                || {
                    rds(
                        &router,
                        &runtime,
                        "DescribeDBClusterSnapshots",
                        &[p("DBClusterSnapshotIdentifier", SNAPSHOT)],
                    )
                    .served("describe the DB cluster snapshot")
                },
                |answer| element(answer, "Status") == "available",
            );
            let snapshot_arn = element(&snapshot, "DBClusterSnapshotArn");

            // ---- and each export's manifest ----
            for export in EXPORTS {
                let (source_arn, source_type) = match export.from_the_snapshot {
                    true => (snapshot_arn.clone(), "SNAPSHOT"),
                    false => (cluster_arn.clone(), "CLUSTER"),
                };
                let mut params = vec![
                    p("ExportTaskIdentifier", export.identifier),
                    p("SourceArn", &source_arn),
                    p("S3BucketName", BUCKET),
                    p("IamRoleArn", &role_arn),
                    p("KmsKeyId", &kms_key_id),
                ];
                if let Some(prefix) = export.prefix {
                    params.push(p("S3Prefix", prefix));
                }
                // The awsQuery form of a list member is `ExportOnly.member.<n>`, from one.
                for (index, entry) in export.export_only.iter().enumerate() {
                    params.push(p(&format!("ExportOnly.member.{}", index + 1), entry));
                }
                rds(&router, &runtime, "StartExportTask", &params).served(export.case);

                // MILESTONE: the export reaches a terminal status, which is what a client
                // polls for before it goes looking for the objects.
                let described = poll_until(
                    export.case,
                    || {
                        rds(
                            &router,
                            &runtime,
                            "DescribeExportTasks",
                            &[p("ExportTaskIdentifier", export.identifier)],
                        )
                        .served(export.case)
                    },
                    |answer| {
                        ["COMPLETE", "FAILED", "CANCELED"]
                            .contains(&element(answer, "Status").as_str())
                    },
                );
                assert_eq!(
                    (
                        element(&described, "Status"),
                        opt_element(&described, "FailureCause")
                    ),
                    ("COMPLETE".to_string(), None),
                    "case {:?}: an export that wrote its objects is COMPLETE with no failure \
                     cause, got {described}",
                    export.case
                );

                // Where every object of this export is: the export's own folder below the
                // prefix the request named, and the bucket's root for a request that named
                // none.
                let under = match export.prefix {
                    Some(prefix) => format!("{prefix}/{}/", export.identifier),
                    None => format!("{}/", export.identifier),
                };
                let objects = listed_objects(&router, &runtime, &under);

                // Both manifests are there, under the keys the user guide names them by.
                let summary_key = format!("{under}export_info_{}.json", export.identifier);
                let tables_key_prefix =
                    format!("{under}export_tables_info_{}_from_1_to_", export.identifier);
                assert!(
                    objects.iter().any(|(key, _)| key == &summary_key),
                    "case {:?}: the export writes its summary at {summary_key:?}, got {objects:?}",
                    export.case
                );
                let tables_key = objects
                    .iter()
                    .find(|(key, _)| key.starts_with(&tables_key_prefix) && key.ends_with(".json"))
                    .map(|(key, _)| key.clone())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: the export writes its per-table file under \
                             {tables_key_prefix:?}, got {objects:?}",
                            export.case
                        )
                    });

                // The SUMMARY describes the export that ran: its identity, its source, its
                // destination as the request named it, and the key its data is under.
                let summary = manifest_document(&router, &runtime, export.case, &summary_key);
                let described_export = [
                    ("exportTaskIdentifier", export.identifier.to_string()),
                    ("sourceArn", source_arn.clone()),
                    ("sourceType", source_type.to_string()),
                    ("s3Bucket", BUCKET.to_string()),
                    ("s3Prefix", export.prefix.unwrap_or_default().to_string()),
                    ("kmsKeyId", kms_key_id.clone()),
                    ("status", "COMPLETE".to_string()),
                ];
                for (member, expected) in &described_export {
                    assert_eq!(
                        summary[member].as_str(),
                        Some(expected.as_str()),
                        "case {:?}: the summary's {member} is what the export ran with, got \
                         {summary}",
                        export.case
                    );
                }
                assert_eq!(
                    summary["percentProgress"].as_u64(),
                    Some(100),
                    "case {:?}: a completed export's summary reports all of it done, got \
                     {summary}",
                    export.case
                );
                let reported_only: Vec<String> = summary["exportOnly"]
                    .as_array()
                    .unwrap_or_else(|| panic!("the summary carries its exportOnly: {summary}"))
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect();
                assert_eq!(
                    reported_only, export.export_only,
                    "case {:?}: the summary reports the ExportOnly the request carried, got \
                     {summary}",
                    export.case
                );

                // And it lists what was WRITTEN: this database's tables with the rows they
                // really hold, each naming an object the destination bucket really holds.
                let summary_tables = reported_tables(&summary, "tables");
                for seed in SEEDS {
                    let expected = export
                        .tables
                        .contains(&seed.table)
                        .then_some(seed.rows);
                    let found = summary_tables
                        .iter()
                        .find(|(database, table, _, _)| {
                            database == DATABASE && table == seed.table
                        })
                        .map(|(_, _, rows, _)| *rows);
                    assert_eq!(
                        found, expected,
                        "case {:?} / table {:?}: the summary reports the rows that table holds \
                         for a table the export covered and nothing for one it did not, got \
                         {summary_tables:?}",
                        export.case, seed.table
                    );
                }
                if !export.other_databases {
                    let outside: Vec<&(String, String, u64, Vec<String>)> = summary_tables
                        .iter()
                        .filter(|(database, _, _, _)| database != DATABASE)
                        .collect();
                    assert!(
                        outside.is_empty(),
                        "case {:?}: an ExportOnly naming one database covers no other, got \
                         {outside:?}",
                        export.case
                    );
                }
                assert_eq!(
                    summary["tablesExported"].as_u64(),
                    Some(summary_tables.len() as u64),
                    "case {:?}: the summary counts the tables it lists, got {summary}",
                    export.case
                );
                assert_eq!(
                    summary["rowsExported"].as_u64(),
                    Some(summary_tables.iter().map(|(_, _, rows, _)| rows).sum()),
                    "case {:?}: the summary's row total is the rows of the tables it lists, \
                     got {summary}",
                    export.case
                );
                for (database, table, _, files) in &summary_tables {
                    for file in files {
                        let held = objects.iter().find(|(key, _)| key == file);
                        let Some((_, size)) = held else {
                            panic!(
                                "case {:?} / {database}.{table}: the manifest names an object \
                                 the bucket holds, got {objects:?}",
                                export.case
                            );
                        };
                        assert!(
                            *size > 0,
                            "case {:?} / {database}.{table}: {file} carries the table's rows, \
                             so it is not empty",
                            export.case
                        );
                    }
                }

                // The PER-TABLE file beside it describes the same export and the same tables:
                // two manifests of one export cannot disagree about what it covered.
                let per_table = manifest_document(&router, &runtime, export.case, &tables_key);
                for (member, expected) in &described_export[..4] {
                    assert_eq!(
                        per_table[member].as_str(),
                        Some(expected.as_str()),
                        "case {:?}: the per-table file's {member} is the export's own, got \
                         {per_table}",
                        export.case
                    );
                }
                let per_table_tables: Vec<(String, String, u64)> =
                    reported_tables(&per_table, "perTableStatus")
                        .into_iter()
                        .filter(|(database, _, _, _)| database == DATABASE)
                        .map(|(database, table, rows, _)| (database, table, rows))
                        .collect();
                let summary_of_this_database: Vec<(String, String, u64)> = summary_tables
                    .iter()
                    .filter(|(database, _, _, _)| database == DATABASE)
                    .map(|(database, table, rows, _)| (database.clone(), table.clone(), *rows))
                    .collect();
                assert_eq!(
                    per_table_tables, summary_of_this_database,
                    "case {:?}: both manifests of one export report the same tables and rows",
                    export.case
                );
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to its assertions");
}
