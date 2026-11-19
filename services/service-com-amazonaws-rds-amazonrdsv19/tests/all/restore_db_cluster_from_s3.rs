// SPDX-License-Identifier: CC0-1.0

//! What `RestoreDBClusterFromS3` accepts, and what it refuses.
//!
//! The rules this suite pins, each over a table spanning what it generalises over:
//!
//! * The migration reads ONE engine pair: it "creates an Amazon Aurora DB cluster from MySQL
//!   data stored in an Amazon S3 bucket", so the cluster is of the single engine `Engine`
//!   publishes, the backup is of the single engine `SourceEngine` publishes and of a version
//!   `SourceEngineVersion` publishes. Any other pair is two parameters that cannot hold
//!   together — `InvalidParameterCombination` — and it is answered from the REQUEST alone,
//!   before the bucket is read: a row whose bucket holds no backup at all is still refused for
//!   its engines.
//! * A location that holds no file for this Region to migrate leaves the restore nothing to
//!   read, and each condition is refused as Amazon RDS refuses it: a bucket this Region's
//!   object store does not reach — one it holds nothing under, one it holds in another Region
//!   — is the parameter value it is (`InvalidParameterValue`), while a bucket that IS there
//!   holding no backup under the prefix is the modelled `InvalidS3BucketFault`. Both carry
//!   the one sentence that error class states.
//! * A request whose engine pair and backup both resolve CREATES the cluster: it is reported
//!   by `DescribeDBClusters` from the restore onwards, reaches `available` on its own and
//!   publishes an endpoint, whatever the files under the prefix are named.
//! * A refused restore leaves NOTHING behind: the identifier it named is still free.
//!
//! Amazon S3 is mounted beside this service on ONE engine, exactly as the assembled router
//! mounts them, and every object this suite stages is written through Amazon S3's own
//! `PutObject`, so what the restore finds is what that service really holds.

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, put_object};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// A second Region, for the row whose bucket exists outside the restore's own.
const OTHER_REGION: &str = "eu-west-1";

/// The master user every restore below names, and the password it carries — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// The bytes one staged backup file carries — a backup is binary, and nothing here reads it,
/// so the content only has to be non-empty.
const BACKUP_BYTES: &[u8] = b"percona-xtrabackup-emulated-payload";

/// `RestoreDBClusterFromS3` with every required member, `extra` replacing or adding to them.
fn restore_params(identifier: &str, bucket: &str, extra: &[(&'static str, String)]) -> Params {
    let mut full: Params = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => st::S3_RESTORE_CLUSTER.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_CLUSTER.source_engine,
        "SourceEngineVersion" => "8.0.28",
        "S3BucketName" => bucket,
        "S3IngestionRoleArn" => format!("arn:aws:iam::000000000000:role/{identifier}-ingest"),
    ]
    .to_vec();
    for (member, value) in extra {
        match full.iter_mut().find(|(named, _)| named == member) {
            Some(entry) => entry.1 = value.clone(),
            None => full.push((member, value.clone())),
        }
    }
    full
}

/// Assert the identifier `identifier` names no DB cluster: what a refused restore has to
/// leave behind.
async fn no_such_cluster(case: &str, router: &MultiServiceRouter, identifier: &str) {
    expect_error(
        case,
        call(
            router,
            "DescribeDBClusters",
            &params!["DBClusterIdentifier" => identifier],
        )
        .await,
        404,
        "DBClusterNotFoundFault",
    );
}

/// One row of the engine-pair table: the case, the members it replaces, and whether the
/// bucket it names holds a backup at all.
struct EngineRow {
    case: &'static str,
    identifier: &'static str,
    replaced: &'static [(&'static str, &'static str)],
    /// `false` for the rows that prove the pair is decided BEFORE the bucket is read: the
    /// bucket they name holds nothing, and the refusal is still the engine pair's.
    bucket_holds_a_backup: bool,
}

/// Rule: the migration reads the one engine pair its three source members publish, and a
/// request naming any other is refused as the parameter combination it is — decided from the
/// request alone, so a bucket holding no backup does not change the answer.
///
/// The rows span the cluster engine, the source engine and the source engine version, each
/// with a value outside its published domain, including values the issue did not cite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_pair_the_migration_does_not_read_is_refused_before_the_bucket() {
    const ROWS: &[EngineRow] = &[
        EngineRow {
            case: "a source engine of another database product",
            identifier: "emu-s3-restore-src-postgres",
            replaced: &[("SourceEngine", "postgres")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine of the same family but another product",
            identifier: "emu-s3-restore-src-mariadb",
            replaced: &[("SourceEngine", "mariadb")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine naming the Aurora engine rather than the backup's",
            identifier: "emu-s3-restore-src-aurora",
            replaced: &[("SourceEngine", "aurora-mysql")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a cluster engine this operation does not create",
            identifier: "emu-s3-restore-engine-pg",
            replaced: &[("Engine", "aurora-postgresql")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a cluster engine that is no DB cluster engine at all",
            identifier: "emu-s3-restore-engine-mysql",
            replaced: &[("Engine", "mysql")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine version below the published series",
            identifier: "emu-s3-restore-version-56",
            replaced: &[("SourceEngineVersion", "5.6.51")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine version above the published series",
            identifier: "emu-s3-restore-version-84",
            replaced: &[("SourceEngineVersion", "8.4.0")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine version that is no version",
            identifier: "emu-s3-restore-version-text",
            replaced: &[("SourceEngineVersion", "latest")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "both engines wrong, and a bucket holding no backup",
            identifier: "emu-s3-restore-pair-empty",
            replaced: &[("Engine", "aurora-postgresql"), ("SourceEngine", "postgres")],
            bucket_holds_a_backup: false,
        },
        EngineRow {
            case: "a source engine version the backup cannot have, and an empty bucket",
            identifier: "emu-s3-restore-version-empty",
            replaced: &[("SourceEngineVersion", "5.5")],
            bucket_holds_a_backup: false,
        },
    ];

    let (router, _engine) = emulator();
    let stocked = "emu-s3-restore-stocked";
    let empty = "emu-s3-restore-empty";
    create_bucket(&router, stocked, REGION).await;
    create_bucket(&router, empty, REGION).await;
    put_object(
        &router,
        stocked,
        "xtrabackup_info",
        BACKUP_BYTES.to_vec(),
    )
    .await;

    for row in ROWS {
        let bucket = if row.bucket_holds_a_backup {
            stocked
        } else {
            empty
        };
        let replaced: Params = row
            .replaced
            .iter()
            .map(|(member, value)| (*member, (*value).to_string()))
            .collect();
        let body = expect_error(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &restore_params(row.identifier, bucket, &replaced),
            )
            .await,
            400,
            "InvalidParameterCombination",
        );
        // The refusal names the values the request carried, so a caller can fix the request
        // from the sentence alone.
        let message = text(&body, "Message");
        for (_, value) in row.replaced {
            assert!(
                message.contains(value),
                "case {:?}: expected the refusal to name {value:?}, got {message:?}",
                row.case
            );
        }
        no_such_cluster(row.case, &router, row.identifier).await;
    }
}

/// One row of the backup-location table: the case, the bucket and prefix the request names,
/// the objects the location holds, and the refusal that location carries.
struct LocationRow {
    case: &'static str,
    identifier: &'static str,
    bucket: &'static str,
    prefix: Option<&'static str>,
    /// The Query API error code the location is refused with: which code says whether the
    /// value named a bucket this Region's object store holds at all.
    code: &'static str,
}

/// Rule: a location holding no file this Region can migrate leaves the restore nothing to
/// read, and each of the two conditions is refused the way Amazon RDS refuses it — a bucket
/// this Region's object store does not reach is the parameter value it is
/// (`InvalidParameterValue`), while a bucket that IS there holding no backup under the prefix
/// is the modelled `InvalidS3BucketFault`. Both carry the one sentence that error class
/// states for this operation.
///
/// The rows span every way a location can hold none: no such bucket, a bucket outside the
/// Region, an empty bucket, and a prefix with nothing under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_location_holding_no_backup_is_refused_as_that_location_is() {
    const ELSEWHERE: &str = "emu-s3-restore-elsewhere";
    const PREFIXED: &str = "emu-s3-restore-prefixed";
    const EMPTY: &str = "emu-s3-restore-no-objects";
    const ROWS: &[LocationRow] = &[
        LocationRow {
            case: "a bucket the object store holds nothing under",
            identifier: "emu-s3-restore-absent",
            bucket: "emu-s3-restore-never-created",
            prefix: None,
            code: "InvalidParameterValue",
        },
        LocationRow {
            case: "a bucket that holds no object at all",
            identifier: "emu-s3-restore-void",
            bucket: EMPTY,
            prefix: None,
            code: "InvalidS3BucketFault",
        },
        LocationRow {
            case: "a prefix the backup is not under",
            identifier: "emu-s3-restore-wrong-prefix",
            bucket: PREFIXED,
            prefix: Some("ledger/"),
            code: "InvalidS3BucketFault",
        },
        LocationRow {
            case: "a bucket that exists in another Region",
            identifier: "emu-s3-restore-cross-region",
            bucket: ELSEWHERE,
            prefix: None,
            code: "InvalidParameterValue",
        },
    ];

    let (router, _engine) = emulator();
    create_bucket(&router, EMPTY, REGION).await;
    create_bucket(&router, PREFIXED, REGION).await;
    create_bucket(&router, ELSEWHERE, OTHER_REGION).await;
    put_object(
        &router,
        PREFIXED,
        "backups/xtrabackup_info",
        BACKUP_BYTES.to_vec(),
    )
    .await;
    put_object(&router, ELSEWHERE, "xtrabackup_info", BACKUP_BYTES.to_vec()).await;

    for row in ROWS {
        let extra: Params = row
            .prefix
            .map(|prefix| params!["S3Prefix" => prefix].to_vec())
            .unwrap_or_default();
        let body = expect_error(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &restore_params(row.identifier, row.bucket, &extra),
            )
            .await,
            400,
            row.code,
        );
        // The sentence that error class states for this operation, which names the two
        // members a caller can act on and carries no value of the request.
        let message = text(&body, "Message");
        let expected = "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't \
                        authorized to access the specified Amazon S3 bucket. Verify the \
                        SourceS3BucketName and S3IngestionRoleArn values and try again.";
        assert_eq!(
            message, expected,
            "case {:?}: expected the sentence {expected:?}, got {message:?}",
            row.case
        );
        no_such_cluster(row.case, &router, row.identifier).await;
    }
}

/// One row of the accepted-restore table: the case, the object the backup is staged as, the
/// prefix the request names and the source engine spelling and version it carries.
struct AcceptedRow {
    case: &'static str,
    identifier: &'static str,
    object_key: &'static str,
    prefix: Option<&'static str>,
    source_engine: &'static str,
    source_engine_version: &'static str,
}

/// Rule: a request whose engine pair and backup location both resolve CREATES the DB cluster
/// — it is reported by `DescribeDBClusters` from the restore onwards and reaches `available`
/// on its own.
///
/// The rows span the forms a Percona XtraBackup backup takes in a bucket (the tool's metadata
/// files, a streamed backup, a compressed streamed backup) and a file named like none of them,
/// a prefix given and omitted, and the spellings and published series of the source engine and
/// its version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resolvable_request_restores_the_cluster_and_reports_it() {
    const ROWS: &[AcceptedRow] = &[
        AcceptedRow {
            case: "a backup directory at the root of the bucket",
            identifier: "emu-s3-restore-root",
            object_key: "xtrabackup_info",
            prefix: None,
            source_engine: "mysql",
            source_engine_version: "8.0.28",
        },
        AcceptedRow {
            case: "a backup directory under a prefix",
            identifier: "emu-s3-restore-under-prefix",
            object_key: "backups/2026/xtrabackup_checkpoints",
            prefix: Some("backups/"),
            source_engine: "mysql",
            source_engine_version: "5.7.40",
        },
        AcceptedRow {
            case: "the tool's configuration file as the backup's marker",
            identifier: "emu-s3-restore-my-cnf",
            object_key: "nightly/backup-my.cnf",
            prefix: Some("nightly"),
            source_engine: "MySQL",
            source_engine_version: "5.7",
        },
        AcceptedRow {
            case: "a streamed backup, one object",
            identifier: "emu-s3-restore-stream",
            object_key: "stream/full-backup.xbstream",
            prefix: Some("stream"),
            source_engine: "mysql",
            source_engine_version: "8.0",
        },
        AcceptedRow {
            case: "a compressed streamed backup",
            identifier: "emu-s3-restore-archive",
            object_key: "archive/full-backup.tar.gz",
            prefix: Some("archive"),
            source_engine: "MYSQL",
            source_engine_version: "8.0.36",
        },
        AcceptedRow {
            case: "files named like no backup tool's, which the migration reads afterwards",
            identifier: "emu-s3-restore-opaque",
            object_key: "opaque/0001-of-0002",
            prefix: Some("opaque"),
            source_engine: "mysql",
            source_engine_version: "8.0.39",
        },
    ];

    let (router, _engine) = emulator();
    let bucket = "emu-s3-restore-accepted";
    create_bucket(&router, bucket, REGION).await;
    for row in ROWS {
        put_object(&router, bucket, row.object_key, BACKUP_BYTES.to_vec()).await;
    }

    for row in ROWS {
        let mut extra: Params = params![
            "SourceEngine" => row.source_engine,
            "SourceEngineVersion" => row.source_engine_version,
            "DatabaseName" => "appdb",
        ]
        .to_vec();
        if let Some(prefix) = row.prefix {
            extra.push(("S3Prefix", prefix.to_string()));
        }
        let body = expect_ok(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &restore_params(row.identifier, bucket, &extra),
            )
            .await,
        );
        // The answer is the cluster the restore created, in the state a restore starts in.
        for (member, expected) in [
            ("DBClusterIdentifier", row.identifier),
            ("Engine", st::S3_RESTORE_CLUSTER.engine),
            ("MasterUsername", MASTER_USER),
            ("DatabaseName", "appdb"),
            ("Status", st::DB_CLUSTER_STATUS_CREATING),
        ] {
            let actual = text(&body, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?}",
                row.case
            );
        }
        // The restored cluster is READ BACK through the operation that reports one: it reaches
        // `available` on its own, and the report carries the endpoint a client opens a session
        // on and the port the engine's own default gives it.
        let reported = crate::lifecycle_poll_loops::poll_until(
            row.case,
            "the restored cluster reaches available",
            || async {
                call(
                    &router,
                    "DescribeDBClusters",
                    &params!["DBClusterIdentifier" => row.identifier],
                )
                .await
            },
            |status, body| {
                status == 200 && text(body, "Status") == st::DB_CLUSTER_STATUS_AVAILABLE
            },
        )
        .await
        .0;
        for (member, expected) in [
            ("DBClusterIdentifier", row.identifier.to_string()),
            ("Engine", st::S3_RESTORE_CLUSTER.engine.to_string()),
        ] {
            let actual = text(&reported, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?}",
                row.case
            );
        }
        // The address a client opens a session on: the host the cluster publishes and the
        // port its own engine listener really answers on.
        let port = text(&reported, "Port");
        assert!(
            port.parse::<u16>().is_ok_and(|port| port > 0),
            "case {:?}: expected the restored cluster to publish the port its engine listens \
             on, got {port:?}",
            row.case
        );
        let endpoint = text(&reported, "Endpoint");
        assert!(
            !endpoint.is_empty(),
            "case {:?}: expected the restored cluster to publish an endpoint, got {endpoint:?}",
            row.case
        );

        // And the identifier is TAKEN from the restore onwards: a second restore of the same
        // backup onto it is the modelled conflict rather than a second cluster.
        expect_error(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &restore_params(row.identifier, bucket, &extra),
            )
            .await,
            400,
            "DBClusterAlreadyExistsFault",
        );
    }
}
