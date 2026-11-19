// SPDX-License-Identifier: CC0-1.0

//! What `RestoreDBInstanceFromS3` accepts, and what it refuses.
//!
//! The same two rules the sibling `RestoreDBClusterFromS3` suite pins, applied to the
//! operation that restores a DB INSTANCE from a database backed up into Amazon S3 — because
//! the rule is one rule and both operations are accepted against it:
//!
//! * The migration reads ONE engine pair, the one this operation's `Engine`, `SourceEngine`
//!   and `SourceEngineVersion` members publish. Any other is two parameters that cannot hold
//!   together — `InvalidParameterCombination` — answered from the REQUEST alone, before the
//!   bucket is read.
//! * A location holding no file this Region can migrate is refused as that location is: a
//!   bucket this Region's object store does not reach is `InvalidParameterValue`, and a
//!   bucket that IS there holding no backup under the prefix is the modelled
//!   `InvalidS3BucketFault`; both carry the one sentence that error class states.
//! * A request whose engine pair and backup both resolve CREATES the instance, which
//!   `DescribeDBInstances` then reports, reaching `available` on its own.

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, put_object};
use service_com_amazonaws_rds_amazonrdsv19::state as st;

/// A second Region, for the row whose bucket exists outside the restore's own.
const OTHER_REGION: &str = "eu-west-1";

/// The master user every restore below names, and the password it carries — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The instance class every restore below asks for.
const INSTANCE_CLASS: &str = "db.t3.micro";

/// The bytes one staged backup file carries — a backup is binary, and nothing here reads it,
/// so the content only has to be non-empty.
const BACKUP_BYTES: &[u8] = b"percona-xtrabackup-emulated-payload";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// `RestoreDBInstanceFromS3` with every required member, `extra` replacing or adding to them.
fn restore_params(identifier: &str, bucket: &str, extra: &[(&'static str, String)]) -> Params {
    let mut full: Params = params![
        "DBInstanceIdentifier" => identifier,
        "DBInstanceClass" => INSTANCE_CLASS,
        "Engine" => st::S3_RESTORE_INSTANCE.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_INSTANCE.source_engine,
        "SourceEngineVersion" => "5.7.40",
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

/// One row of the engine-pair table: the case, the members it replaces, and whether the bucket
/// it names holds a backup at all.
struct EngineRow {
    case: &'static str,
    identifier: &'static str,
    replaced: &'static [(&'static str, &'static str)],
    /// `false` for the rows that prove the pair is decided BEFORE the bucket is read.
    bucket_holds_a_backup: bool,
}

/// Rule: the migration reads the one engine pair this operation's members publish, and any
/// other is refused as the parameter combination it is — before the bucket is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_pair_the_instance_migration_does_not_read_is_refused() {
    const ROWS: &[EngineRow] = &[
        EngineRow {
            case: "a source engine of another database product",
            identifier: "emu-s3-inst-src-postgres",
            replaced: &[("SourceEngine", "postgres")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine of the same family but another product",
            identifier: "emu-s3-inst-src-mariadb",
            replaced: &[("SourceEngine", "mariadb")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "an engine this operation does not create an instance of",
            identifier: "emu-s3-inst-engine-pg",
            replaced: &[("Engine", "postgres")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "an engine naming the Aurora cluster engine rather than the instance's",
            identifier: "emu-s3-inst-engine-aurora",
            replaced: &[("Engine", "aurora-mysql")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine version below the published series",
            identifier: "emu-s3-inst-version-55",
            replaced: &[("SourceEngineVersion", "5.5.62")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "a source engine version above the published series",
            identifier: "emu-s3-inst-version-80",
            replaced: &[("SourceEngineVersion", "8.0.36")],
            bucket_holds_a_backup: true,
        },
        EngineRow {
            case: "both engines wrong, and a bucket holding no backup",
            identifier: "emu-s3-inst-pair-empty",
            replaced: &[("Engine", "postgres"), ("SourceEngine", "postgres")],
            bucket_holds_a_backup: false,
        },
    ];

    let (router, _engine) = emulator();
    let stocked = "emu-s3-inst-stocked";
    let empty = "emu-s3-inst-empty";
    create_bucket(&router, stocked, REGION).await;
    create_bucket(&router, empty, REGION).await;
    put_object(&router, stocked, "xtrabackup_info", BACKUP_BYTES.to_vec()).await;

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
                "RestoreDBInstanceFromS3",
                &restore_params(row.identifier, bucket, &replaced),
            )
            .await,
            400,
            "InvalidParameterCombination",
        );
        let message = text(&body, "Message");
        for (_, value) in row.replaced {
            assert!(
                message.contains(value),
                "case {:?}: expected the refusal to name {value:?}, got {message:?}",
                row.case
            );
        }
        // A refused restore leaves nothing behind: the identifier it named is still free.
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => row.identifier],
            )
            .await,
            404,
            "DBInstanceNotFound",
        );
    }
}

/// One row of the backup-location table: the case, the bucket and prefix the request names,
/// and the refusal that location carries.
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
/// read, and each of the two conditions is refused the way Amazon RDS refuses it — the rule
/// the sibling cluster suite pins, which holds for this operation too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_location_holding_no_backup_is_refused_as_that_location_is() {
    const ELSEWHERE: &str = "emu-s3-inst-elsewhere";
    const PREFIXED: &str = "emu-s3-inst-prefixed";
    const ROWS: &[LocationRow] = &[
        LocationRow {
            case: "a bucket the object store holds nothing under",
            identifier: "emu-s3-inst-absent",
            bucket: "emu-s3-inst-never-created",
            prefix: None,
            code: "InvalidParameterValue",
        },
        LocationRow {
            case: "a prefix the backup is not under",
            identifier: "emu-s3-inst-wrong-prefix",
            bucket: PREFIXED,
            prefix: Some("ledger/"),
            code: "InvalidS3BucketFault",
        },
        LocationRow {
            case: "a bucket that exists in another Region",
            identifier: "emu-s3-inst-cross-region",
            bucket: ELSEWHERE,
            prefix: None,
            code: "InvalidParameterValue",
        },
    ];

    let (router, _engine) = emulator();
    create_bucket(&router, PREFIXED, REGION).await;
    create_bucket(&router, ELSEWHERE, OTHER_REGION).await;
    put_object(
        &router,
        PREFIXED,
        "backups/xtrabackup_checkpoints",
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
                "RestoreDBInstanceFromS3",
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
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => row.identifier],
            )
            .await,
            404,
            "DBInstanceNotFound",
        );
    }
}

/// One row of the accepted-restore table: the case, the object the backup is staged as, the
/// prefix the request names, and the spelling and version of the source engine it carries.
struct AcceptedRow {
    case: &'static str,
    identifier: &'static str,
    object_key: &'static str,
    prefix: Option<&'static str>,
    source_engine: &'static str,
    source_engine_version: &'static str,
}

/// Rule: a request whose engine pair and backup location both resolve CREATES the DB instance
/// — `DescribeDBInstances` reports it from the restore onwards and it reaches `available` on
/// its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resolvable_request_restores_the_instance_and_reports_it() {
    const ROWS: &[AcceptedRow] = &[
        AcceptedRow {
            case: "a backup directory at the root of the bucket",
            identifier: "emu-s3-inst-root",
            object_key: "xtrabackup_info",
            prefix: None,
            source_engine: "mysql",
            source_engine_version: "5.7.40",
        },
        AcceptedRow {
            case: "a backup directory under a prefix, on the older published series",
            identifier: "emu-s3-inst-under-prefix",
            object_key: "backups/2026/xtrabackup_checkpoints",
            prefix: Some("backups/"),
            source_engine: "MySQL",
            source_engine_version: "5.6.40",
        },
        AcceptedRow {
            case: "a streamed backup, one object",
            identifier: "emu-s3-inst-stream",
            object_key: "stream/full-backup.xbstream",
            prefix: Some("stream"),
            source_engine: "mysql",
            source_engine_version: "5.7",
        },
        AcceptedRow {
            case: "files named like no backup tool's, which the migration reads afterwards",
            identifier: "emu-s3-inst-opaque",
            object_key: "opaque/0001-of-0002",
            prefix: Some("opaque"),
            source_engine: "mysql",
            source_engine_version: "5.7.44",
        },
    ];

    let (router, _engine) = emulator();
    let bucket = "emu-s3-inst-accepted";
    create_bucket(&router, bucket, REGION).await;
    for row in ROWS {
        put_object(&router, bucket, row.object_key, BACKUP_BYTES.to_vec()).await;
    }

    for row in ROWS {
        let mut extra: Params = params![
            "SourceEngine" => row.source_engine,
            "SourceEngineVersion" => row.source_engine_version,
            "DBName" => "appdb",
        ]
        .to_vec();
        if let Some(prefix) = row.prefix {
            extra.push(("S3Prefix", prefix.to_string()));
        }
        let body = expect_ok(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &restore_params(row.identifier, bucket, &extra),
            )
            .await,
        );
        for (member, expected) in [
            ("DBInstanceIdentifier", row.identifier),
            ("Engine", st::S3_RESTORE_INSTANCE.engine),
            ("DBInstanceClass", INSTANCE_CLASS),
            ("MasterUsername", MASTER_USER),
            ("DBName", "appdb"),
            ("DBInstanceStatus", st::DB_INSTANCE_STATUS_CREATING),
        ] {
            let actual = text(&body, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?}",
                row.case
            );
        }
        // The restored instance is READ BACK through the operation that reports one: it
        // reaches `available` on its own and publishes the address a client opens a session
        // on.
        let reported = crate::lifecycle_poll_loops::poll_until(
            row.case,
            "the restored instance reaches available",
            || async {
                call(
                    &router,
                    "DescribeDBInstances",
                    &params!["DBInstanceIdentifier" => row.identifier],
                )
                .await
            },
            |status, body| {
                status == 200
                    && text(body, "DBInstanceStatus") == st::DB_INSTANCE_STATUS_AVAILABLE
            },
        )
        .await
        .0;
        let address = text(&reported, "Address");
        assert!(
            !address.is_empty(),
            "case {:?}: expected the restored instance to publish an endpoint address, got \
             {address:?}",
            row.case
        );
        let port = text(&reported, "Port");
        assert!(
            port.parse::<u16>().is_ok_and(|port| port > 0),
            "case {:?}: expected the restored instance to publish the port its engine listens \
             on, got {port:?}",
            row.case
        );

        // And the identifier is TAKEN from the restore onwards: a second restore of the same
        // backup onto it is the modelled conflict rather than a second instance.
        expect_error(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &restore_params(row.identifier, bucket, &extra),
            )
            .await,
            400,
            "DBInstanceAlreadyExists",
        );
    }
}
