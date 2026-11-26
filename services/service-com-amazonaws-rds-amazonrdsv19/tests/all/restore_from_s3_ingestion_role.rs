// SPDX-License-Identifier: CC0-1.0

//! WHO a restore from Amazon S3 reads its backup as: the ingestion role, and what IAM says
//! that role may do.
//!
//! The rule both restore-from-Amazon-S3 operations are accepted against: `S3IngestionRoleArn`
//! is the IAM role "that authorizes Amazon RDS to access the Amazon S3 bucket on your behalf"
//! — "a trust policy and a permissions policy that allows Amazon RDS to access your Amazon S3
//! bucket" — so the restore is accepted against that role in TWO stages, which answer
//! different things to the caller:
//!
//! * whether the value is a role the restore can be run under AT ALL — a role ARN of this
//!   account that IAM holds and whose trust lets Amazon RDS act as it. It is asked BEFORE the
//!   source location is resolved, since there is no identity to read that location under, and a
//!   value that is not one is refused as the parameter value it is (`InvalidParameterValue`)
//!   with the sentence naming the permission set the role is required for;
//! * and then whether the permissions in force on that role cover the Amazon S3 reads the
//!   migration makes on the location that resolved: the listing that finds the backup's files
//!   (`s3:ListBucket`, on the bucket) and the read of each file it found (`s3:GetObject`, on the
//!   object), the two the user guide's ingestion policy grants. Where IAM answers that the role
//!   withholds one of them, the restore is refused with the sentence Amazon RDS words files it
//!   cannot list in; where IAM answers nothing about the role, nothing is refused.
//!
//! No resource is created for a restore refused at either stage.
//!
//! Each rule is asserted over a TABLE whose rows differ in WHAT the role's policy set states —
//! which of the two actions it grants, which resources it grants them on, whether it denies
//! one outright, whether the role states permissions at all and whether IAM holds the role —
//! and the rule is pinned for BOTH operations, the DB cluster restore and the DB instance
//! restore, because it is one rule.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: every bucket and object is written through Amazon S3's own operations
//! and every role and policy through IAM's own, so what the restore resolves is what those
//! services really hold.

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::rds_s3_integration::{create_bucket, emulator, put_object, role};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The service principal an ingestion role trusts: Amazon RDS acts under it.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The master user every restore below names, and the password it carries — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The instance class the DB instance rows ask for.
const INSTANCE_CLASS: &str = "db.t3.micro";

/// The bytes one staged backup file carries — a backup is binary and nothing here reads it, so
/// the content only has to be non-empty.
const BACKUP_BYTES: &[u8] = b"percona-xtrabackup-emulated-payload";

/// The prefix every restore below names, and the backup file staged under it.
const PREFIX: &str = "backups/";
const BACKUP_KEY: &str = "backups/xtrabackup_info";

/// The sentence Amazon RDS refuses a restore whose ingestion role cannot read the backup
/// with: fixed text, with no value of the request in it.
const CANNOT_LIST: &str = "Files from the specified Amazon S3 bucket cannot be listed. Make \
                           sure that you have created an AWS Identity and Access Management \
                           (IAM) role that lets Amazon RDS access Amazon S3 for you.";

/// The sentence Amazon RDS refuses a value that is not a role the restore can be run under
/// with: it states that the ARN is invalid or does not carry what is required, and names the
/// permission set it is required for. Fixed text, naming no value of the request.
const UNUSABLE_ROLE: &str = "IAM role ARN value is invalid or does not include the required \
                             permissions for: S3_SNAPSHOT_INGESTION";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// What IAM holds for one row's ingestion role.
enum RolePolicySet {
    /// IAM holds NO role under that name: the ARN names a role of this account that is not
    /// there, so Amazon RDS has no identity to read the backup under.
    NoRole,
    /// The role is there, trusting a service that is not Amazon RDS: Amazon RDS cannot act as
    /// it, whatever its permissions say.
    TrustsAnotherService,
    /// The value names no role of THIS account at all — it is used as written, so a row can
    /// carry an ARN of another account or a value that is no ARN.
    NotThisAccountsRole(&'static str),
    /// The role is there and carries no identity-based policy at all — the routine state of a
    /// role provisioned out of band, so its policy set states nothing about the reads.
    NoPolicy,
    /// The role carries this inline document, built around the bucket under test.
    Document(fn(&str) -> String),
}

/// The ingestion policy the Amazon RDS User Guide asks for ("Creating an IAM role manually",
/// Migrating data from MySQL by using an Amazon S3 bucket): `s3:ListBucket` on the bucket and
/// `s3:GetObject` on the objects in it.
fn grants_the_whole_read(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:ListBucket","s3:GetObject"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}}]}}"#
    )
}

/// The same policy with the LISTING left out: the role can read an object it is told about but
/// cannot find the backup's files.
fn grants_the_object_read_only(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetObject"],"Resource":["arn:aws:s3:::{bucket}/*"]}}]}}"#
    )
}

/// The same policy with the OBJECT read left out: the role can list the backup's files but not
/// read one of them.
fn grants_the_listing_only(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::{bucket}"]}}]}}"#
    )
}

/// A policy that states permissions on the bucket but none of the reads the migration makes.
fn grants_neither_read(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetBucketLocation","s3:PutObject"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}}]}}"#
    )
}

/// The whole read, granted on ANOTHER bucket: a grant that does not reach the location this
/// restore names.
fn grants_the_whole_read_elsewhere(_bucket: &str) -> String {
    grants_the_whole_read("emu-ingest-some-other-bucket")
}

/// The whole read granted AND the listing denied outright: an explicit deny is an answer about
/// the role whatever else it carries.
fn denies_the_listing(bucket: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:ListBucket","s3:GetObject"],"Resource":["arn:aws:s3:::{bucket}","arn:aws:s3:::{bucket}/*"]}},{{"Effect":"Deny","Action":"s3:ListBucket","Resource":"arn:aws:s3:::{bucket}"}}]}}"#
    )
}

/// Every Amazon S3 action on every resource, as an administrative role carries it.
fn grants_every_action(_bucket: &str) -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]}"#
        .to_string()
}

/// One row of either table: the case, the identifier the restore names, and the policy set IAM
/// holds for the role it reads the backup under.
struct Row {
    case: &'static str,
    identifier: &'static str,
    role_name: &'static str,
    policies: RolePolicySet,
}

/// Provision the row's ingestion role in IAM and answer with the ARN the restore names it by.
async fn ingestion_role(router: &MultiServiceRouter, row: &Row, bucket: &str) -> String {
    match &row.policies {
        RolePolicySet::NoRole => format!(
            "arn:aws:iam::{}:role/{}",
            emulator_server::emulator_account_id(),
            row.role_name
        ),
        RolePolicySet::NotThisAccountsRole(value) => (*value).to_string(),
        RolePolicySet::TrustsAnotherService => {
            role(router, row.role_name, "ec2.amazonaws.com", None).await
        }
        RolePolicySet::NoPolicy => role(router, row.role_name, RDS_PRINCIPAL, None).await,
        RolePolicySet::Document(document) => {
            role(
                router,
                row.role_name,
                RDS_PRINCIPAL,
                Some(document(bucket)),
            )
            .await
        }
    }
}

/// `RestoreDBClusterFromS3` with every required member, plus the prefix and the ingestion role
/// this suite's rows differ in.
fn cluster_params(identifier: &str, bucket: &str, role_arn: &str) -> Params {
    params![
        "DBClusterIdentifier" => identifier,
        "Engine" => st::S3_RESTORE_CLUSTER.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_CLUSTER.source_engine,
        "SourceEngineVersion" => "8.0.28",
        "S3BucketName" => bucket,
        "S3Prefix" => PREFIX,
        "S3IngestionRoleArn" => role_arn,
    ]
    .to_vec()
}

/// `RestoreDBInstanceFromS3` with every required member, the same way.
fn instance_params(identifier: &str, bucket: &str, role_arn: &str) -> Params {
    params![
        "DBInstanceIdentifier" => identifier,
        "DBInstanceClass" => INSTANCE_CLASS,
        "Engine" => st::S3_RESTORE_INSTANCE.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_INSTANCE.source_engine,
        "SourceEngineVersion" => "5.7.40",
        "S3BucketName" => bucket,
        "S3Prefix" => PREFIX,
        "S3IngestionRoleArn" => role_arn,
    ]
    .to_vec()
}

/// Stage the backup every row of one table reads: a bucket in this Region holding one file
/// under the prefix, so the only thing a row can be refused for is its ROLE.
async fn staged_backup(router: &MultiServiceRouter, bucket: &str) {
    create_bucket(router, bucket, REGION).await;
    put_object(router, bucket, BACKUP_KEY, BACKUP_BYTES.to_vec()).await;
}

/// Assert the identifier names no DB cluster: what a refused restore has to leave behind.
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

/// Assert the identifier names no DB instance.
async fn no_such_instance(case: &str, router: &MultiServiceRouter, identifier: &str) {
    expect_error(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
        404,
        "DBInstanceNotFound",
    );
}

/// Assert one row's refusal: the code, the sentence and that it carries no value of the
/// request.
fn expect_cannot_list(case: &str, response: (u16, String)) {
    let body = expect_error(case, response, 400, "InvalidParameterValue");
    let message = text(&body, "Message");
    assert_eq!(
        message, CANNOT_LIST,
        "case {case:?}: expected the sentence {CANNOT_LIST:?}, got {message:?}"
    );
}

/// Assert one row's refusal of the ROLE VALUE itself: the same code, the sentence that names
/// the permission set the role is required for, and no value of the request in it.
fn expect_unusable_role(case: &str, response: (u16, String)) {
    let body = expect_error(case, response, 400, "InvalidParameterValue");
    let message = text(&body, "Message");
    assert_eq!(
        message, UNUSABLE_ROLE,
        "case {case:?}: expected the sentence {UNUSABLE_ROLE:?}, got {message:?}"
    );
}

/// Rule: a value that is not a role this restore can be run under at all — one IAM holds no
/// such role of this account under, one whose trust does not let Amazon RDS act as it, one
/// naming another account's role, and one that is no role ARN — leaves the restore no identity
/// to read the backup under, so it is refused with the sentence that names the permission set
/// the role is required for, and NOTHING is created.
///
/// It is refused BEFORE the source location is resolved, which the rows prove by naming a
/// bucket this Region's object store holds nothing under: the location's own refusal never
/// reaches the caller. The rule is pinned for both restores, because it is one rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_value_that_is_no_usable_ingestion_role_refuses_both_restores() {
    const ROWS: &[Row] = &[
        Row {
            case: "a role ARN of this account IAM holds nothing under",
            identifier: "emu-ingest-absent",
            role_name: "emu-ingest-absent-role",
            policies: RolePolicySet::NoRole,
        },
        Row {
            case: "a role whose trust policy admits another service only",
            identifier: "emu-ingest-untrusting",
            role_name: "emu-ingest-untrusting-role",
            policies: RolePolicySet::TrustsAnotherService,
        },
        Row {
            case: "a well-formed role ARN of another account",
            identifier: "emu-ingest-other-account",
            role_name: "emu-ingest-other-account-role",
            policies: RolePolicySet::NotThisAccountsRole(
                "arn:aws:iam::210987654321:role/emu-ingest-elsewhere",
            ),
        },
        Row {
            case: "a value that is no ARN at all",
            identifier: "emu-ingest-not-an-arn",
            role_name: "emu-ingest-not-an-arn-role",
            policies: RolePolicySet::NotThisAccountsRole("emu-ingest-just-a-name"),
        },
        Row {
            case: "an ARN of another service's resource",
            identifier: "emu-ingest-not-a-role",
            role_name: "emu-ingest-not-a-role-role",
            policies: RolePolicySet::NotThisAccountsRole(
                "arn:aws:s3:::emu-ingest-cluster-refused",
            ),
        },
    ];

    let (router, _engine) = emulator();
    // A bucket this Region's object store holds NOTHING under: a restore that got past the
    // role would be refused for the location, so the sentence each row asserts is proof that
    // the role is read first.
    let bucket = "emu-ingest-never-created";

    for row in ROWS {
        let role_arn = ingestion_role(&router, row, bucket).await;
        expect_unusable_role(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &cluster_params(row.identifier, bucket, &role_arn),
            )
            .await,
        );
        no_such_cluster(row.case, &router, row.identifier).await;

        let instance_identifier = format!("{}-inst", row.identifier);
        expect_unusable_role(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &instance_params(&instance_identifier, bucket, &role_arn),
            )
            .await,
        );
        no_such_instance(row.case, &router, &instance_identifier).await;
    }
}

/// Rule: the Amazon S3 reads a restore from Amazon S3 makes are authorized against the
/// ingestion role's permissions, so a role IAM answers withholds the listing of the bucket or
/// the read of the backup's files leaves the DB cluster restore refused with
/// `InvalidParameterValue` and no cluster behind it.
///
/// The rows span which of the two actions the policy set grants, the resource the grant
/// reaches, and an explicit deny of one of them — including cases the issue did not cite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ingestion_role_that_cannot_read_the_backup_refuses_the_cluster_restore() {
    const ROWS: &[Row] = &[
        Row {
            case: "a role that can read an object but cannot list the bucket",
            identifier: "emu-ingest-no-list",
            role_name: "emu-ingest-no-list-role",
            policies: RolePolicySet::Document(grants_the_object_read_only),
        },
        Row {
            case: "a role that can list the bucket but cannot read the backup's files",
            identifier: "emu-ingest-no-get",
            role_name: "emu-ingest-no-get-role",
            policies: RolePolicySet::Document(grants_the_listing_only),
        },
        Row {
            case: "a role whose policy states other actions on the same bucket",
            identifier: "emu-ingest-other-actions",
            role_name: "emu-ingest-other-actions-role",
            policies: RolePolicySet::Document(grants_neither_read),
        },
        Row {
            case: "a role granted the whole read on another bucket",
            identifier: "emu-ingest-other-bucket",
            role_name: "emu-ingest-other-bucket-role",
            policies: RolePolicySet::Document(grants_the_whole_read_elsewhere),
        },
        Row {
            case: "a role granted the whole read and denied the listing outright",
            identifier: "emu-ingest-denied",
            role_name: "emu-ingest-denied-role",
            policies: RolePolicySet::Document(denies_the_listing),
        },
    ];

    let (router, _engine) = emulator();
    let bucket = "emu-ingest-cluster-refused";
    staged_backup(&router, bucket).await;

    for row in ROWS {
        let role_arn = ingestion_role(&router, row, bucket).await;
        expect_cannot_list(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &cluster_params(row.identifier, bucket, &role_arn),
            )
            .await,
        );
        no_such_cluster(row.case, &router, row.identifier).await;
    }
}

/// Rule, the other side: a role IAM does not answer against — one whose permissions cover both
/// reads, and one IAM says nothing about at all — leaves the restore to run, and the cluster it
/// creates is reported by `DescribeDBClusters` from that moment on.
///
/// The rows span a grant written for this bucket, a grant of every action, and a role IAM
/// holds that carries no policy at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ingestion_role_iam_does_not_refuse_restores_the_cluster() {
    const ROWS: &[Row] = &[
        Row {
            case: "a role granted the listing and the object read on this bucket",
            identifier: "emu-ingest-granted",
            role_name: "emu-ingest-granted-role",
            policies: RolePolicySet::Document(grants_the_whole_read),
        },
        Row {
            case: "a role granted every Amazon S3 action",
            identifier: "emu-ingest-admin",
            role_name: "emu-ingest-admin-role",
            policies: RolePolicySet::Document(grants_every_action),
        },
        Row {
            case: "a role that states no permissions at all",
            identifier: "emu-ingest-silent",
            role_name: "emu-ingest-silent-role",
            policies: RolePolicySet::NoPolicy,
        },
    ];

    let (router, _engine) = emulator();
    let bucket = "emu-ingest-cluster-accepted";
    staged_backup(&router, bucket).await;

    for row in ROWS {
        let role_arn = ingestion_role(&router, row, bucket).await;
        let body = expect_ok(
            row.case,
            call(
                &router,
                "RestoreDBClusterFromS3",
                &cluster_params(row.identifier, bucket, &role_arn),
            )
            .await,
        );
        let identifier = text(&body, "DBClusterIdentifier");
        assert_eq!(
            identifier, row.identifier,
            "case {:?}: expected the restore to answer with the cluster it created, got \
             {identifier:?}",
            row.case
        );
        // READ BACK through the operation that reports a DB cluster: the restore wrote a
        // cluster this service really holds.
        let reported = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBClusters",
                &params!["DBClusterIdentifier" => row.identifier],
            )
            .await,
        );
        for (member, expected) in [
            ("DBClusterIdentifier", row.identifier),
            ("Engine", st::S3_RESTORE_CLUSTER.engine),
            ("MasterUsername", MASTER_USER),
        ] {
            let actual = text(&reported, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?}",
                row.case
            );
        }
    }
}

/// Rule: it is ONE rule, so the DB instance restore is accepted against the same reads — a
/// role that withholds one of them is refused with the same code and sentence and leaves no
/// instance, and a role that holds both restores the instance `DescribeDBInstances` then
/// reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ingestion_role_rule_holds_for_the_instance_restore() {
    const REFUSED: &[Row] = &[
        Row {
            case: "an instance restore under a role that cannot list the bucket",
            identifier: "emu-ingest-inst-no-list",
            role_name: "emu-ingest-inst-no-list-role",
            policies: RolePolicySet::Document(grants_the_object_read_only),
        },
        Row {
            case: "an instance restore under a role that cannot read the backup's files",
            identifier: "emu-ingest-inst-no-get",
            role_name: "emu-ingest-inst-no-get-role",
            policies: RolePolicySet::Document(grants_the_listing_only),
        },
        Row {
            case: "an instance restore under a role denied the listing outright",
            identifier: "emu-ingest-inst-denied",
            role_name: "emu-ingest-inst-denied-role",
            policies: RolePolicySet::Document(denies_the_listing),
        },
    ];
    const ACCEPTED: &[Row] = &[
        Row {
            case: "an instance restore under a role granted the whole read",
            identifier: "emu-ingest-inst-granted",
            role_name: "emu-ingest-inst-granted-role",
            policies: RolePolicySet::Document(grants_the_whole_read),
        },
        Row {
            case: "an instance restore under a role that states no permissions at all",
            identifier: "emu-ingest-inst-silent",
            role_name: "emu-ingest-inst-silent-role",
            policies: RolePolicySet::NoPolicy,
        },
    ];

    let (router, _engine) = emulator();
    let bucket = "emu-ingest-instance-backup";
    staged_backup(&router, bucket).await;

    for row in REFUSED {
        let role_arn = ingestion_role(&router, row, bucket).await;
        expect_cannot_list(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &instance_params(row.identifier, bucket, &role_arn),
            )
            .await,
        );
        no_such_instance(row.case, &router, row.identifier).await;
    }

    for row in ACCEPTED {
        let role_arn = ingestion_role(&router, row, bucket).await;
        let body = expect_ok(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &instance_params(row.identifier, bucket, &role_arn),
            )
            .await,
        );
        let identifier = text(&body, "DBInstanceIdentifier");
        assert_eq!(
            identifier, row.identifier,
            "case {:?}: expected the restore to answer with the instance it created, got \
             {identifier:?}",
            row.case
        );
        let reported = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBInstances",
                &params!["DBInstanceIdentifier" => row.identifier],
            )
            .await,
        );
        for (member, expected) in [
            ("DBInstanceIdentifier", row.identifier),
            ("Engine", st::S3_RESTORE_INSTANCE.engine),
            ("MasterUsername", MASTER_USER),
        ] {
            let actual = text(&reported, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {member} {expected:?}, got {actual:?}",
                row.case
            );
        }
    }
}
