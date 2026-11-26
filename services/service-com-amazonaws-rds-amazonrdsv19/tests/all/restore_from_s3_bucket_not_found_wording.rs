// SPDX-License-Identifier: CC0-1.0

//! WHOSE words a refused source bucket is reported in: the operation that answers the refusal.
//!
//! The rule: a refusal message belongs to the operation that answers it. The two
//! restore-from-Amazon-S3 operations both refuse a source bucket they cannot reach with
//! `InvalidParameterValue` at HTTP 400, and each words that cause its own way —
//! `RestoreDBClusterFromS3` states that the bucket cannot be found and points at the IAM role
//! that lets Amazon RDS reach Amazon S3, while `RestoreDBInstanceFromS3` names the two members
//! of ITS own request (`SourceS3BucketName` and `S3IngestionRoleArn`) to verify. Neither
//! sentence is the other's, and neither interpolates anything: both are fixed text, so no
//! bucket name, prefix or role ARN of the request appears in either.
//!
//! The rows span what the rule generalises over: both operations, both ways a bucket is out of
//! reach (this Region's object store holds nothing under the name, and it holds a bucket of
//! that name in another Region), bucket names of different shape and length, and a request
//! that names a prefix beside one that names none. Every row reads under an ingestion role IAM
//! answers holds the whole read of every bucket, so nothing a row is refused for is the role's
//! doing, and every row is checked to have created no resource.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: every bucket and role is written through that service's own operations.

use crate::db_instances::{call, expect_error, text};
use crate::rds_s3_integration::{create_bucket, emulator, role};
use service_com_amazonaws_rds_amazonrdsv19::state as st;

/// A second Region, for the rows whose bucket exists outside the restore's own.
const OTHER_REGION: &str = "eu-west-1";

/// The service principal an ingestion role trusts: Amazon RDS acts under it.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The master user every restore below names, and the password it carries — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The instance class the DB instance rows ask for.
const INSTANCE_CLASS: &str = "db.t3.micro";

/// The sentence `RestoreDBClusterFromS3` refuses an unreachable source bucket with, spelled
/// out here rather than taken from the code under test.
const CLUSTER_SENTENCE: &str =
    "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS \
     Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you.";

/// The sentence `RestoreDBInstanceFromS3` refuses the same cause with.
const INSTANCE_SENTENCE: &str =
    "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to \
     access the specified Amazon S3 bucket. Verify the SourceS3BucketName and \
     S3IngestionRoleArn values and try again.";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a row can build them.
type Params = Vec<(&'static str, String)>;

/// The ingestion policy the Amazon RDS User Guide asks an ingestion role to carry ("Creating
/// an IAM role manually", Migrating data from MySQL by using an Amazon S3 bucket):
/// `s3:ListBucket` on the bucket and `s3:GetObject` on the objects in it — granted here on
/// EVERY bucket, so a row's refusal is never the role's doing.
fn grants_the_whole_read() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:ListBucket","s3:GetObject"],"Resource":"*"}]}"#
        .to_string()
}

/// A bucket this Region's object store holds nothing under, named plainly.
const ABSENT: &str = "emu-og-never-created";
/// A second absent bucket, named in the longest dotted form Amazon S3 allows, so the sentence
/// is pinned against a bucket name of another shape and length.
const ABSENT_DOTTED: &str = "emu.og.absent.bucket.with.a.much.longer.name.0123456789";
/// A bucket that exists, in another Region, so this Region's restore cannot reach it.
const ELSEWHERE: &str = "emu-og-elsewhere";

/// One row: the case, which operation answers, the bucket and prefix the request names, and
/// the sentence that operation words the refusal with.
struct Row {
    case: &'static str,
    /// `true` for `RestoreDBClusterFromS3`, `false` for `RestoreDBInstanceFromS3`.
    cluster: bool,
    identifier: &'static str,
    bucket: &'static str,
    prefix: Option<&'static str>,
    sentence: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the cluster restore, a bucket nothing was created under",
        cluster: true,
        identifier: "emu-og-cluster-absent",
        bucket: ABSENT,
        prefix: None,
        sentence: CLUSTER_SENTENCE,
    },
    Row {
        case: "the cluster restore, an absent bucket named in dotted form, under a prefix",
        cluster: true,
        identifier: "emu-og-cluster-dotted",
        bucket: ABSENT_DOTTED,
        prefix: Some("backups/2026/06/"),
        sentence: CLUSTER_SENTENCE,
    },
    Row {
        case: "the cluster restore, a bucket of another Region",
        cluster: true,
        identifier: "emu-og-cluster-elsewhere",
        bucket: ELSEWHERE,
        prefix: Some("xtrabackup/"),
        sentence: CLUSTER_SENTENCE,
    },
    Row {
        case: "the instance restore, a bucket nothing was created under",
        cluster: false,
        identifier: "emu-og-instance-absent",
        bucket: ABSENT,
        prefix: None,
        sentence: INSTANCE_SENTENCE,
    },
    Row {
        case: "the instance restore, an absent bucket named in dotted form, under a prefix",
        cluster: false,
        identifier: "emu-og-instance-dotted",
        bucket: ABSENT_DOTTED,
        prefix: Some("backups/2026/06/"),
        sentence: INSTANCE_SENTENCE,
    },
    Row {
        case: "the instance restore, a bucket of another Region",
        cluster: false,
        identifier: "emu-og-instance-elsewhere",
        bucket: ELSEWHERE,
        prefix: Some("xtrabackup/"),
        sentence: INSTANCE_SENTENCE,
    },
];

impl Row {
    /// The operation this row calls.
    fn action(&self) -> &'static str {
        if self.cluster {
            "RestoreDBClusterFromS3"
        } else {
            "RestoreDBInstanceFromS3"
        }
    }

    /// The request this row makes: every required member of its operation, plus the location
    /// and the ingestion role it reads under.
    fn request(&self, role_arn: &str) -> Params {
        let mut request: Params = if self.cluster {
            params![
                "DBClusterIdentifier" => self.identifier,
                "Engine" => st::S3_RESTORE_CLUSTER.engine,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "SourceEngine" => st::S3_RESTORE_CLUSTER.source_engine,
                "SourceEngineVersion" => "8.0.32",
                "S3BucketName" => self.bucket,
                "S3IngestionRoleArn" => role_arn,
            ]
            .to_vec()
        } else {
            params![
                "DBInstanceIdentifier" => self.identifier,
                "DBInstanceClass" => INSTANCE_CLASS,
                "Engine" => st::S3_RESTORE_INSTANCE.engine,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "SourceEngine" => st::S3_RESTORE_INSTANCE.source_engine,
                "SourceEngineVersion" => "8.0.32",
                "S3BucketName" => self.bucket,
                "S3IngestionRoleArn" => role_arn,
            ]
            .to_vec()
        };
        if let Some(prefix) = self.prefix {
            request.push(("S3Prefix", prefix.to_string()));
        }
        request
    }

    /// The read that answers for the resource this row's restore would have created, and the
    /// not-found fault it answers when nothing was created.
    fn describe(&self) -> (&'static str, &'static str, &'static str) {
        if self.cluster {
            (
                "DescribeDBClusters",
                "DBClusterIdentifier",
                "DBClusterNotFoundFault",
            )
        } else {
            (
                "DescribeDBInstances",
                "DBInstanceIdentifier",
                "DBInstanceNotFound",
            )
        }
    }
}

/// Rule: an unreachable source bucket is refused with `InvalidParameterValue` at HTTP 400 and
/// the sentence the OPERATION THAT ANSWERS the refusal words that cause with — fixed text that
/// names no value the request carried — and no resource is created for the refused request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_restore_refuses_an_unreachable_bucket_in_its_own_words() {
    let (router, _engine) = emulator();
    create_bucket(&router, ELSEWHERE, OTHER_REGION).await;
    let role_arn = role(
        &router,
        "emu-og-ingestion-role",
        RDS_PRINCIPAL,
        Some(grants_the_whole_read()),
    )
    .await;

    for row in ROWS {
        let case = row.case;
        let body = expect_error(
            case,
            call(&router, row.action(), &row.request(&role_arn)).await,
            400,
            "InvalidParameterValue",
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, row.sentence,
            "case {case:?}: expected the sentence {:?}, got {message:?}",
            row.sentence
        );
        for value in [Some(row.bucket), row.prefix, Some(role_arn.as_str())]
            .into_iter()
            .flatten()
        {
            assert!(
                !message.contains(value),
                "case {case:?}: expected fixed text naming no value of the request, but \
                 {message:?} carries {value:?}"
            );
        }

        // Nothing was created for the refused request: the identifier it named is still free.
        let (describe, member, not_found) = row.describe();
        expect_error(
            case,
            call(&router, describe, &params![member => row.identifier]).await,
            404,
            not_found,
        );
    }
}

/// Rule, stated as the difference it makes: the two operations do NOT share one sentence for
/// this cause, so a client that tells the causes of the one error code apart by the message
/// reads the cluster restore's own words from the cluster restore.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_two_restores_word_an_unreachable_bucket_differently() {
    let (router, _engine) = emulator();
    let role_arn = role(
        &router,
        "emu-og-compare-role",
        RDS_PRINCIPAL,
        Some(grants_the_whole_read()),
    )
    .await;
    let cluster = Row {
        case: "the cluster restore, for the comparison",
        cluster: true,
        identifier: "emu-og-compare-cluster",
        bucket: ABSENT,
        prefix: None,
        sentence: CLUSTER_SENTENCE,
    };
    let instance = Row {
        case: "the instance restore, for the comparison",
        cluster: false,
        identifier: "emu-og-compare-instance",
        bucket: ABSENT,
        prefix: None,
        sentence: INSTANCE_SENTENCE,
    };

    let mut answered = Vec::new();
    for row in [&cluster, &instance] {
        let body = expect_error(
            row.case,
            call(&router, row.action(), &row.request(&role_arn)).await,
            400,
            "InvalidParameterValue",
        );
        answered.push(text(&body, "Message"));
    }
    assert_ne!(
        answered[0], answered[1],
        "case {:?}: expected the two operations to word this cause apart, both answered {:?}",
        "one bucket, two operations", answered[0]
    );
    assert_eq!(
        answered[0], CLUSTER_SENTENCE,
        "case {:?}: expected the cluster restore's own sentence, got {:?}",
        cluster.case, answered[0]
    );
    assert_eq!(
        answered[1], INSTANCE_SENTENCE,
        "case {:?}: expected the instance restore's own sentence, got {:?}",
        instance.case, answered[1]
    );
}
