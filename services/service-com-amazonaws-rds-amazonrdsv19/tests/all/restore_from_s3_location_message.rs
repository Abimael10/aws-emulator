// SPDX-License-Identifier: CC0-1.0

//! WHICH cause of `InvalidS3BucketFault` a restore from Amazon S3 reports: the message.
//!
//! The rule both restore-from-Amazon-S3 operations are accepted against: one error class
//! covers several distinct causes of a source location, so the CODE alone does not say what a
//! caller has to fix and the MESSAGE is the diagnosis. A location the restore cannot find or
//! read at all carries the sentence the OPERATION THAT ANSWERS IT words that cause with — the
//! cluster restore stating the bucket is not there and pointing at the IAM role to create, the
//! DB instance restore naming the bucket-name and ingestion-role members of its own request to
//! verify; a location that IS there and readable but holds no backup files for the migration
//! to read carries the sentence that reports the missing backup files and asks for them to be
//! created — the files themselves are the one thing missing, so neither the bucket name nor
//! the role is what the caller is pointed at, and both operations word that cause alike.
//!
//! Each sentence is fixed text: it names no value the request carried, so no bucket name, no
//! prefix and no role ARN appears in it.
//!
//! The rule is pinned over a TABLE whose rows differ in HOW the location holds no backup —
//! a bucket this Region's object store does not reach, a bucket it reaches that is empty, a
//! bucket holding objects under no prefix the request named, and a prefix deeper than the one
//! the backup is under — and for BOTH operations, the DB cluster restore and the DB instance
//! restore, because it is one rule. Every row reads under an ingestion role IAM answers holds
//! the whole read of the bucket, so nothing a row is refused for is the role's doing.
//!
//! The last assertion is the positive control: staging a backup file under the prefix of the
//! very bucket a row was refused for leaves the SAME request accepted, so what the refusal
//! reported really was the absence of the files.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: every bucket, object, role and policy is written through that service's
//! own operations.

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, put_object, role};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// A second Region, for the rows whose bucket lives outside the restore's own.
const OTHER_REGION: &str = "eu-west-1";

/// The service principal an ingestion role trusts: Amazon RDS acts under it.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The master user every restore below names, and the password it carries — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The instance class the DB instance rows ask for.
const INSTANCE_CLASS: &str = "db.t3.micro";

/// The bytes one staged file carries — nothing here reads a backup, so the content only has to
/// be non-empty.
const FILE_BYTES: &[u8] = b"percona-xtrabackup-emulated-payload";

/// The sentence `RestoreDBClusterFromS3` words a source location it cannot find or read at all
/// with: the bucket is not there, and the remedy is the IAM role that lets Amazon RDS reach
/// Amazon S3.
const CLUSTER_BUCKET_UNREACHABLE: &str =
    "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS \
     Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you.";

/// The sentence `RestoreDBInstanceFromS3` words that same cause with: it names the two members
/// of ITS request a caller can act on.
const INSTANCE_BUCKET_UNREACHABLE: &str =
    "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to \
     access the specified Amazon S3 bucket. Verify the SourceS3BucketName and \
     S3IngestionRoleArn values and try again.";

/// The sentence Amazon RDS words a bucket that IS there and readable but holds no backup files
/// with: it reports the missing files and the remedy for them.
const NO_BACKUP_FILES: &str = "No backup files were found in the specified Amazon S3 bucket. \
                               Check the bucket name or create backup files and try again.";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// The ingestion policy the Amazon RDS User Guide asks an ingestion role to carry ("Creating
/// an IAM role manually", Migrating data from MySQL by using an Amazon S3 bucket):
/// `s3:ListBucket` on the bucket and `s3:GetObject` on the objects in it — granted here on
/// EVERY bucket, so a row's refusal is never the role's doing.
fn grants_the_whole_read() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:ListBucket","s3:GetObject"],"Resource":"*"}]}"#
        .to_string()
}

/// One row of the location table: the case, the bucket and prefix the request names, and the
/// refusal that location carries — the code and the sentence of ITS cause.
struct Row {
    case: &'static str,
    suffix: &'static str,
    bucket: &'static str,
    prefix: Option<&'static str>,
    code: &'static str,
    /// The sentence `RestoreDBClusterFromS3` words this location's refusal with.
    cluster_message: &'static str,
    /// The sentence `RestoreDBInstanceFromS3` words it with — the same text for every cause the
    /// two operations word alike, and its own for a bucket neither of them can reach.
    instance_message: &'static str,
}

/// A bucket this Region's object store holds nothing under.
const ABSENT: &str = "emu-loc-never-created";
/// A bucket this Region holds, left EMPTY — the issue's own setup.
const EMPTY: &str = "emu-loc-empty";
/// A bucket this Region holds, with files under one prefix only.
const PREFIXED: &str = "emu-loc-prefixed";
/// A bucket that exists, in another Region.
const ELSEWHERE: &str = "emu-loc-elsewhere";

/// The prefix the files of [`PREFIXED`] are under.
const BACKUP_PREFIX: &str = "backups/";

const ROWS: &[Row] = &[
    Row {
        case: "a bucket the object store holds nothing under",
        suffix: "absent",
        bucket: ABSENT,
        prefix: None,
        code: "InvalidParameterValue",
        cluster_message: CLUSTER_BUCKET_UNREACHABLE,
        instance_message: INSTANCE_BUCKET_UNREACHABLE,
    },
    Row {
        case: "a bucket that exists in another Region",
        suffix: "elsewhere",
        bucket: ELSEWHERE,
        prefix: None,
        code: "InvalidParameterValue",
        cluster_message: CLUSTER_BUCKET_UNREACHABLE,
        instance_message: INSTANCE_BUCKET_UNREACHABLE,
    },
    Row {
        case: "an existing, readable bucket left empty",
        suffix: "empty",
        bucket: EMPTY,
        prefix: None,
        code: "InvalidS3BucketFault",
        cluster_message: NO_BACKUP_FILES,
        instance_message: NO_BACKUP_FILES,
    },
    Row {
        case: "an existing, readable bucket left empty, under a prefix",
        suffix: "empty-prefixed",
        bucket: EMPTY,
        prefix: Some(BACKUP_PREFIX),
        code: "InvalidS3BucketFault",
        cluster_message: NO_BACKUP_FILES,
        instance_message: NO_BACKUP_FILES,
    },
    Row {
        case: "a bucket holding files under no prefix the request named",
        suffix: "other-prefix",
        bucket: PREFIXED,
        prefix: Some("ledger/"),
        code: "InvalidS3BucketFault",
        cluster_message: NO_BACKUP_FILES,
        instance_message: NO_BACKUP_FILES,
    },
    Row {
        case: "a prefix deeper than the one the files are under",
        suffix: "deeper-prefix",
        bucket: PREFIXED,
        prefix: Some("backups/2026/"),
        code: "InvalidS3BucketFault",
        cluster_message: NO_BACKUP_FILES,
        instance_message: NO_BACKUP_FILES,
    },
];

/// Stage every location the rows read: the buckets this Region holds and the files in them.
async fn staged_locations(router: &MultiServiceRouter) {
    create_bucket(router, EMPTY, REGION).await;
    create_bucket(router, PREFIXED, REGION).await;
    create_bucket(router, ELSEWHERE, OTHER_REGION).await;
    put_object(
        router,
        PREFIXED,
        "backups/xtrabackup_info",
        FILE_BYTES.to_vec(),
    )
    .await;
    put_object(router, ELSEWHERE, "xtrabackup_info", FILE_BYTES.to_vec()).await;
}

/// `RestoreDBClusterFromS3` with every required member, plus the location and the ingestion
/// role a row reads under.
fn cluster_params(identifier: &str, row: &Row, role_arn: &str) -> Params {
    let mut request: Params = params![
        "DBClusterIdentifier" => identifier,
        "Engine" => st::S3_RESTORE_CLUSTER.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_CLUSTER.source_engine,
        "SourceEngineVersion" => "8.0.28",
        "S3BucketName" => row.bucket,
        "S3IngestionRoleArn" => role_arn,
    ]
    .to_vec();
    if let Some(prefix) = row.prefix {
        request.push(("S3Prefix", prefix.to_string()));
    }
    request
}

/// `RestoreDBInstanceFromS3` with every required member, the same way.
fn instance_params(identifier: &str, row: &Row, role_arn: &str) -> Params {
    let mut request: Params = params![
        "DBInstanceIdentifier" => identifier,
        "DBInstanceClass" => INSTANCE_CLASS,
        "Engine" => st::S3_RESTORE_INSTANCE.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_INSTANCE.source_engine,
        "SourceEngineVersion" => "5.7.40",
        "S3BucketName" => row.bucket,
        "S3IngestionRoleArn" => role_arn,
    ]
    .to_vec();
    if let Some(prefix) = row.prefix {
        request.push(("S3Prefix", prefix.to_string()));
    }
    request
}

/// Assert one row's refusal: the code of its cause, the sentence the operation under test
/// words that cause with, and that the sentence carries no value the request named.
fn expect_refusal(case: &str, row: &Row, expected: &str, response: (u16, String)) {
    let body = expect_error(case, response, 400, row.code);
    let message = text(&body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the sentence {expected:?}, got {message:?}"
    );
    for value in [Some(row.bucket), row.prefix].into_iter().flatten() {
        assert!(
            !message.contains(value),
            "case {case:?}: expected fixed text naming no value of the request, but \
             {message:?} carries {value:?}"
        );
    }
}

/// Rule: which cause of the one error class a refused source location reports is said by the
/// MESSAGE — the missing-backup-files sentence for a bucket that is there and readable, the
/// cannot-be-found-or-read sentence for one that is not — pinned for `RestoreDBClusterFromS3`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cluster_restore_reports_which_cause_refused_the_location() {
    let (router, _engine) = emulator();
    staged_locations(&router).await;
    let role_arn = role(
        &router,
        "emu-loc-cluster-role",
        RDS_PRINCIPAL,
        Some(grants_the_whole_read()),
    )
    .await;

    for row in ROWS {
        let identifier = format!("emu-loc-cluster-{}", row.suffix);
        expect_refusal(
            row.case,
            row,
            row.cluster_message,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &cluster_params(&identifier, row, &role_arn),
            )
            .await,
        );
        // A refused restore leaves nothing behind: the identifier it named is still free.
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBClusters",
                &params!["DBClusterIdentifier" => identifier],
            )
            .await,
            404,
            "DBClusterNotFoundFault",
        );
    }
}

/// Rule, the same one: it holds for `RestoreDBInstanceFromS3` too, because the two restores
/// read a source location the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_instance_restore_reports_which_cause_refused_the_location() {
    let (router, _engine) = emulator();
    staged_locations(&router).await;
    let role_arn = role(
        &router,
        "emu-loc-instance-role",
        RDS_PRINCIPAL,
        Some(grants_the_whole_read()),
    )
    .await;

    for row in ROWS {
        let identifier = format!("emu-loc-instance-{}", row.suffix);
        expect_refusal(
            row.case,
            row,
            row.instance_message,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &instance_params(&identifier, row, &role_arn),
            )
            .await,
        );
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => identifier],
            )
            .await,
            404,
            "DBInstanceNotFound",
        );
    }
}

/// Rule, the positive control: the missing-backup-files refusal really is about the FILES, so
/// staging one under the prefix of the bucket a row was refused for leaves the same request
/// accepted — and the instance it creates is reported by `DescribeDBInstances`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staging_the_backup_files_accepts_the_restore_that_was_refused() {
    const CASE: &str = "the empty bucket, once a backup file is under the prefix";
    let (router, _engine) = emulator();
    staged_locations(&router).await;
    let role_arn = role(
        &router,
        "emu-loc-control-role",
        RDS_PRINCIPAL,
        Some(grants_the_whole_read()),
    )
    .await;
    let row = Row {
        case: CASE,
        suffix: "control",
        bucket: EMPTY,
        prefix: Some(BACKUP_PREFIX),
        code: "InvalidS3BucketFault",
        cluster_message: NO_BACKUP_FILES,
        instance_message: NO_BACKUP_FILES,
    };

    expect_refusal(
        CASE,
        &row,
        row.instance_message,
        call(
            &router,
            "RestoreDBInstanceFromS3",
            &instance_params("emu-loc-control", &row, &role_arn),
        )
        .await,
    );

    put_object(
        &router,
        EMPTY,
        "backups/xtrabackup_checkpoints",
        FILE_BYTES.to_vec(),
    )
    .await;

    let body = expect_ok(
        CASE,
        call(
            &router,
            "RestoreDBInstanceFromS3",
            &instance_params("emu-loc-control", &row, &role_arn),
        )
        .await,
    );
    let answered = text(&body, "DBInstanceIdentifier");
    assert_eq!(
        answered, "emu-loc-control",
        "case {CASE:?}: expected the restore to answer with the instance it created, got \
         {answered:?}"
    );
    let reported = expect_ok(
        CASE,
        call(
            &router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => "emu-loc-control"],
        )
        .await,
    );
    for (member, expected) in [
        ("DBInstanceIdentifier", "emu-loc-control"),
        ("Engine", st::S3_RESTORE_INSTANCE.engine),
        ("MasterUsername", MASTER_USER),
    ] {
        let actual = text(&reported, member);
        assert_eq!(
            actual, expected,
            "case {CASE:?}: expected {member} {expected:?}, got {actual:?}"
        );
    }
}
