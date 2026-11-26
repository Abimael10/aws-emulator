// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a restore from Amazon S3 resolves the resources the request
//! NAMES, relative to the source it would read.
//!
//! RULE: a request that names a resource of THIS service resolves it before anything about
//! the source of the migration is read, so a name that matches no such resource of the
//! request's Region is answered with that resource type's own modelled fault — HTTP 404
//! `DBSubnetGroupNotFoundFault` for a `DBSubnetGroupName` nothing holds — and not with the
//! refusal of the bucket, the prefix or the ingestion role the call would have gone on to
//! read. A client branches on the status and the code to decide whether to retry and which
//! resource to create, so a missing DB subnet group must never arrive as a 400 about Amazon S3.
//!
//! The sentence is the one each operation words that fault with: the restore that creates a DB
//! CLUSTER places the cluster in the group and says "DB subnet group '<name>' does not
//! exist.", while the restore that creates a DB INSTANCE words the same fault the way every
//! call that places an instance does — "DBSubnetGroup '<name>' not found." Both are
//! requirements, and the group is named in the folded spelling the member is resolved in
//! ("This value is stored as a lowercase string").
//!
//! The rows span what ELSE about the source is wrong — a bucket this Region's object store
//! holds nothing under, a bucket that IS there and holds no backup file, an ingestion role
//! this account's IAM holds nothing under, and all of them at once — because the rule is about
//! which refusal is answered FIRST and must hold whatever the other defects are. The last
//! assertion is the positive control: with the DB subnet group created, the very same request
//! is answered by the source's own refusal instead, so the 404 really was the group's.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: every bucket, object and role is written through that service's own
//! operations.

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

/// The bytes one staged backup file carries — nothing here reads a backup, so the content only
/// has to be non-empty.
const BACKUP_BYTES: &[u8] = b"percona-xtrabackup-emulated-payload";

/// Two EC2 subnet ids in the two forms the id takes, which between them cover the two
/// Availability Zones a DB subnet group needs.
const TWO_SUBNETS: [&str; 2] = ["subnet-1a2b3c4d", "subnet-0a1b2c3d4e5f60711"];

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, owned so a table row can hold them.
type Params = Vec<(&'static str, String)>;

/// The ingestion policy the Amazon RDS User Guide asks an ingestion role to carry, granted on
/// every bucket.
fn grants_the_whole_read() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:ListBucket","s3:GetObject"],"Resource":"*"}]}"#
        .to_string()
}

/// One row of the table: the case, the DB subnet group the request names, and the state of the
/// source the restore would have read if the group had resolved.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// The `DBSubnetGroupName` the request carries, which no DB subnet group of this Region
    /// answers for.
    named_group: &'static str,
    /// Which staged location the request names.
    bucket: Location,
    /// Whether the ingestion role the request names is one IAM holds.
    role_is_held: bool,
}

/// Which of the staged locations a row's request names.
#[derive(Clone, Copy)]
enum Location {
    /// A bucket this Region's object store holds nothing under.
    Absent,
    /// A bucket that IS there and holds a backup file.
    WithBackup,
    /// A bucket that IS there and holds no file at all.
    Empty,
}

/// A bucket the object store holds nothing under.
const ABSENT: &str = "emu-order-never-created";
/// A bucket this Region holds, with a backup file at its root.
const STOCKED: &str = "emu-order-stocked";
/// A bucket this Region holds, left empty.
const EMPTY: &str = "emu-order-empty";

/// The DB subnet group every row below names and no row creates, until the control does.
const MISSING_GROUP: &str = "emu-order-no-such-subnet-group";

const ROWS: &[Row] = &[
    Row {
        case: "a bucket holding the backup, and a role IAM holds",
        identifier: "emu-order-ready-source",
        named_group: MISSING_GROUP,
        bucket: Location::WithBackup,
        role_is_held: true,
    },
    Row {
        case: "a bucket the object store holds nothing under",
        identifier: "emu-order-absent-bucket",
        named_group: MISSING_GROUP,
        bucket: Location::Absent,
        role_is_held: true,
    },
    Row {
        case: "a bucket that is there and holds no backup file",
        identifier: "emu-order-empty-bucket",
        named_group: MISSING_GROUP,
        bucket: Location::Empty,
        role_is_held: true,
    },
    Row {
        case: "an ingestion role this account's IAM holds nothing under",
        identifier: "emu-order-absent-role",
        named_group: MISSING_GROUP,
        bucket: Location::WithBackup,
        role_is_held: false,
    },
    Row {
        case: "every part of the source wrong at once",
        identifier: "emu-order-nothing-resolves",
        named_group: MISSING_GROUP,
        bucket: Location::Absent,
        role_is_held: false,
    },
    Row {
        case: "a group name carrying the punctuation the member admits",
        identifier: "emu-order-punctuated",
        named_group: "emu-order-subnets-2026",
        bucket: Location::WithBackup,
        role_is_held: true,
    },
];

/// The bucket a row's request names.
fn bucket_of(row: &Row) -> &'static str {
    match row.bucket {
        Location::Absent => ABSENT,
        Location::WithBackup => STOCKED,
        Location::Empty => EMPTY,
    }
}

/// Stage the locations the rows read: the buckets this Region holds and the one backup file in
/// them.
async fn staged_locations(router: &MultiServiceRouter) {
    create_bucket(router, STOCKED, REGION).await;
    create_bucket(router, EMPTY, REGION).await;
    put_object(router, STOCKED, "xtrabackup_info", BACKUP_BYTES.to_vec()).await;
}

/// The ingestion role ARN a row's request carries: a role IAM really holds for the rows that
/// ask for one, and an ARN of this account naming a role IAM holds nothing under otherwise.
async fn ingestion_role(router: &MultiServiceRouter, row: &Row) -> String {
    let name = format!("{}-ingest", row.identifier);
    if row.role_is_held {
        return role(router, &name, RDS_PRINCIPAL, Some(grants_the_whole_read())).await;
    }
    format!(
        "arn:aws:iam::{}:role/{name}",
        emulator_server::emulator_account_id()
    )
}

/// `RestoreDBClusterFromS3` with every required member, plus the DB subnet group the row names
/// and the ingestion role it reads under.
fn cluster_params(row: &Row, role_arn: &str) -> Params {
    params![
        "DBClusterIdentifier" => row.identifier,
        "Engine" => st::S3_RESTORE_CLUSTER.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_CLUSTER.source_engine,
        "SourceEngineVersion" => "8.0.28",
        "S3BucketName" => bucket_of(row),
        "S3IngestionRoleArn" => role_arn,
        "DBSubnetGroupName" => row.named_group,
    ]
    .to_vec()
}

/// `RestoreDBInstanceFromS3` with every required member, the same way.
fn instance_params(identifier: &str, row: &Row, role_arn: &str) -> Params {
    params![
        "DBInstanceIdentifier" => identifier,
        "DBInstanceClass" => INSTANCE_CLASS,
        "Engine" => st::S3_RESTORE_INSTANCE.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "SourceEngine" => st::S3_RESTORE_INSTANCE.source_engine,
        "SourceEngineVersion" => "5.7.44",
        "S3BucketName" => bucket_of(row),
        "S3IngestionRoleArn" => role_arn,
        "DBSubnetGroupName" => row.named_group,
    ]
    .to_vec()
}

/// `CreateDBSubnetGroup` over the two subnets a group must name.
async fn create_subnet_group(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    let request: Params = params![
        "DBSubnetGroupName" => name,
        "DBSubnetGroupDescription" => "the group a restore places its resource in",
        "SubnetIds.SubnetIdentifier.1" => TWO_SUBNETS[0],
        "SubnetIds.SubnetIdentifier.2" => TWO_SUBNETS[1],
    ]
    .to_vec();
    call(router, "CreateDBSubnetGroup", &request).await
}

/// Assert one refusal: the 404 of the DB-subnet-group fault, the sentence the operation words
/// it with, and that the group the request named is in it.
fn expect_group_not_found(case: &str, response: (u16, String), sentence: &str, named: &str) {
    let body = expect_error(case, response, 404, "DBSubnetGroupNotFoundFault");
    let message = text(&body, "Message");
    assert_eq!(
        message, sentence,
        "case {case:?}: expected the sentence {sentence:?}, got {message:?}"
    );
    assert!(
        message.contains(named),
        "case {case:?}: expected the refusal to name the DB subnet group {named:?}, got \
         {message:?}"
    );
}

/// Rule: `RestoreDBClusterFromS3` resolves the DB subnet group the request names before it
/// reads the source, so a group nothing holds is the modelled 404 in this operation's own
/// wording whatever else about the source is wrong — and no cluster is left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cluster_restore_resolves_its_subnet_group_before_the_source() {
    let (router, _engine) = emulator();
    staged_locations(&router).await;

    for row in ROWS {
        let role_arn = ingestion_role(&router, row).await;
        expect_group_not_found(
            row.case,
            call(&router, "RestoreDBClusterFromS3", &cluster_params(row, &role_arn)).await,
            &st::db_subnet_group_does_not_exist_message(row.named_group),
            row.named_group,
        );
        // A refused restore leaves nothing behind: the identifier it named is still free.
        expect_error(
            row.case,
            call(
                &router,
                "DescribeDBClusters",
                &params!["DBClusterIdentifier" => row.identifier],
            )
            .await,
            404,
            "DBClusterNotFoundFault",
        );
    }
}

/// Rule, the same one: `RestoreDBInstanceFromS3` resolves its DB subnet group first too, and
/// words the one fault the way every call that places a DB INSTANCE in a group words it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_instance_restore_resolves_its_subnet_group_before_the_source() {
    let (router, _engine) = emulator();
    staged_locations(&router).await;

    for row in ROWS {
        let role_arn = ingestion_role(&router, row).await;
        let identifier = format!("{}-inst", row.identifier);
        expect_group_not_found(
            row.case,
            call(
                &router,
                "RestoreDBInstanceFromS3",
                &instance_params(&identifier, row, &role_arn),
            )
            .await,
            &st::db_subnet_group_not_found_for_db_instance_message(row.named_group),
            row.named_group,
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

/// Rule, the positive control: the 404 really was about the DB subnet group — once the group
/// is created, the SAME request is answered by the source's own refusal instead (a 400 about
/// the bucket), and a request whose group, source and role all resolve restores the resource.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creating_the_subnet_group_moves_the_refusal_on_to_the_source() {
    const CASE: &str = "the same request, once the DB subnet group exists";
    let (router, _engine) = emulator();
    staged_locations(&router).await;
    expect_ok(CASE, create_subnet_group(&router, MISSING_GROUP).await);

    // The row whose source cannot be read: with the group in place, the refusal is the
    // bucket's, in the class and sentence that location carries.
    let absent_source = Row {
        case: CASE,
        identifier: "emu-order-control-absent",
        named_group: MISSING_GROUP,
        bucket: Location::Absent,
        role_is_held: true,
    };
    let role_arn = ingestion_role(&router, &absent_source).await;
    let body = expect_error(
        CASE,
        call(
            &router,
            "RestoreDBClusterFromS3",
            &cluster_params(&absent_source, &role_arn),
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    let message = text(&body, "Message");
    assert_eq!(
        message,
        st::invalid_ingestion_bucket_message(),
        "case {CASE:?}: expected the location's own sentence, got {message:?}"
    );

    // And the row whose source IS there restores the resource, which the report then carries
    // in the DB subnet group the request named.
    let ready = Row {
        case: CASE,
        identifier: "emu-order-control-ready",
        named_group: MISSING_GROUP,
        bucket: Location::WithBackup,
        role_is_held: true,
    };
    let role_arn = ingestion_role(&router, &ready).await;
    expect_ok(
        CASE,
        call(&router, "RestoreDBClusterFromS3", &cluster_params(&ready, &role_arn)).await,
    );
    let reported = expect_ok(
        CASE,
        call(
            &router,
            "DescribeDBClusters",
            &params!["DBClusterIdentifier" => ready.identifier],
        )
        .await,
    );
    let group = text(&reported, "DBSubnetGroup");
    assert_eq!(
        group, MISSING_GROUP,
        "case {CASE:?}: expected the restored cluster to report the DB subnet group the \
         request named, got {group:?}"
    );
}
