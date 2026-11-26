// SPDX-License-Identifier: CC0-1.0

//! What a backup reports as the SIZE of the backup it took.
//!
//! The rule under test:
//!
//! > `BackupJob$BackupSizeInBytes` is "the size, in bytes, of a backup (recovery point)", and
//! > the member's own documentation states what decides it: the value "can render differently
//! > depending on the resource type as Backup pulls in data information from other Amazon Web
//! > Services services. For example, the value returned may show a value of `0`". So the
//! > figure follows the TYPE of the protected resource — it is one other services report to
//! > Backup — and never a count of the bytes this emulator stored for the resource. The
//! > enumeration that documentation gives reports no figure at all for an Amazon Aurora,
//! > Amazon DocumentDB or Amazon Neptune cluster, an Amazon RDS instance, an Amazon FSx file
//! > system, or an Amazon DynamoDB table without Backup's advanced features (which nothing in
//! > this emulator turns on). A job that settled has moved the whole backup, so its
//! > `BytesTransferred` is that same figure, and the job, the account's job listing, the
//! > recovery point and the vault's listing of it all report the one value settled when the
//! > backup was taken.
//!
//! The rows differ along what the rule generalises over:
//!
//! * how much the protected resource holds when the backup is taken — nothing, a few small
//!   items, one item of a few kilobytes — so no row can pass on a figure counted over what
//!   the emulator stored;
//! * the TYPE of the protected resource, one row per entry of the rule's own catalog
//!   (including the four types this emulator's own issue never cited), so no row can pass on
//!   a case keyed to one recorded request.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire behaviour
//! a `backup` and a `dynamodb` SDK client see. The Backup-and-DynamoDB emulator and the
//! DynamoDB wire are [`crate::restore_job`]'s, the settled-point setup is
//! [`crate::recovery_point_deletion`]'s, and the vault, job and wait helpers are
//! [`crate::vault_and_backup_job`]'s; this suite adds no second copy of any of them.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::recovery_point_deletion::completed_point;
use crate::restore_job::{dynamodb, emulator};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, router, settled_backup_job, start_job, string_at, ACCOUNT,
    REGION,
};

/// `TableStatus` of a table that is usable (model enum `TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// The figure the rule gives a backup whose resource type no service reports a size for.
const NO_SIZE: i64 = 0;

/// How long a wait for one of those conditions runs in all, and how long each look waits
/// before the next: far past the span the emulator's own clock owes a table or a job, so a
/// case never turns on how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_WAIT: Duration = Duration::from_millis(100);

/// One content row: what the protected resource holds when the backup is taken.
struct Holding {
    case: &'static str,
    /// The items put into the table before the backup, as `PutItem` takes them.
    items: Vec<Value>,
}

fn holdings() -> Vec<Holding> {
    vec![
        Holding {
            case: "a table holding nothing",
            items: Vec::new(),
        },
        Holding {
            case: "a table holding a few small items",
            items: vec![
                json!({"pk": {"S": "order-1"}, "total": {"N": "42"}}),
                json!({"pk": {"S": "order-2"}, "total": {"N": "7"}}),
                json!({"pk": {"S": "order-3"}, "owner": {"S": "alice"}}),
            ],
        },
        Holding {
            case: "a table holding one item of a few kilobytes",
            items: vec![json!({
                "pk": {"S": "order-large"},
                "blob": {"S": "x".repeat(4_096)},
            })],
        },
    ]
}

/// One protected-resource-type row: an ARN of that type, and the type the service reads off
/// it.
///
/// SOURCE of the types and of the figure each is expected to carry: the enumeration in the
/// model's documentation for `BackupJob$BackupSizeInBytes`
/// (`smithy_query(query="doc", shape="BackupJob", member="BackupSizeInBytes",
/// model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`),
/// which names each of these types as one no size is populated for.
fn typed_resources() -> Vec<(&'static str, String)> {
    vec![
        (
            "Aurora",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:ledger-cluster"),
        ),
        (
            "DocumentDB",
            format!("arn:aws:docdb:{REGION}:{ACCOUNT}:cluster:catalog-cluster"),
        ),
        (
            "DynamoDB",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders"),
        ),
        (
            "FSx",
            format!("arn:aws:fsx:{REGION}:{ACCOUNT}:file-system/fs-0123456789abcdef0"),
        ),
        (
            "Neptune",
            format!("arn:aws:neptune:{REGION}:{ACCOUNT}:cluster:graph-cluster"),
        ),
        (
            "RDS",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:orders"),
        ),
    ]
}

/// The role a job is started under, as the sibling suites name it.
fn backup_role() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// Create the table `name` through DynamoDB's own wire, wait until that service reports it
/// usable, put `items` into it, and answer the ARN that service gave it.
async fn table_holding(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    items: &[Value],
) -> String {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateTable must succeed, got {status} {created:?}"
    );
    let arn = string_at(
        created.get("TableDescription").unwrap_or(&Value::Null),
        "TableArn",
    );
    active_table(router, case, name).await;
    for item in items {
        let (status, written) =
            dynamodb(router, "PutItem", json!({"TableName": name, "Item": item})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup PutItem must succeed, got {status} {written:?}"
        );
    }
    arn
}

/// The description DynamoDB answers for `name` once it reports the table usable.
///
/// The wait is on the CONDITION, with room well past the span that service's clock takes to
/// create a table.
async fn active_table(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, described) =
            dynamodb(router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable {name} must answer 200, got {status} {described:?}"
        );
        let table = described.get("Table").cloned().unwrap_or(Value::Null);
        if string_at(&table, "TableStatus") == ACTIVE {
            return table;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {name} must become {ACTIVE} within {POLL_DEADLINE:?}, got \
             {described:?}"
        );
        ::tokio::time::sleep(POLL_WAIT).await;
    }
}

/// What the table's OWN service reports about its stored bytes (`TableSizeBytes`), carried
/// into the failure messages: the figure the backup's own size must not be taken from.
async fn table_size_bytes(router: &MultiServiceRouter, case: &str, name: &str) -> i64 {
    active_table(router, case, name)
        .await
        .get("TableSizeBytes")
        .and_then(Value::as_i64)
        .unwrap_or_default()
}

/// The `BackupSizeInBytes` of a member of one answer, with the case named on failure.
fn reported_size(case: &str, what: &str, body: &Value) -> i64 {
    body.get("BackupSizeInBytes")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a BackupSizeInBytes on {what}, got {body}"))
}

/// The settled backup job that wrote `point_arn`, read through the account's job listing.
async fn job_of_point(router: &MultiServiceRouter, case: &str, point_arn: &str) -> Value {
    let (status, listed) = call(router, "GET", "/backup-jobs", None).await;
    assert_eq!(
        (status, error_code(&listed).as_str()),
        (200, "<none>"),
        "case {case:?}: ListBackupJobs must answer 200, got {status} {listed:?}"
    );
    listed
        .get("BackupJobs")
        .and_then(Value::as_array)
        .and_then(|jobs| {
            jobs.iter()
                .find(|job| string_at(job, "RecoveryPointArn") == point_arn)
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must carry the job that wrote {point_arn}, got {listed}")
        })
}

/// The recovery point `point_arn` as the vault's own listing reports it.
async fn listed_point(
    router: &MultiServiceRouter,
    case: &str,
    vault: &str,
    point_arn: &str,
) -> Value {
    let (status, listed) = call(
        router,
        "GET",
        &format!("/backup-vaults/{vault}/recovery-points"),
        None,
    )
    .await;
    assert_eq!(
        (status, error_code(&listed).as_str()),
        (200, "<none>"),
        "case {case:?}: ListRecoveryPointsByBackupVault must answer 200, got {status} {listed:?}"
    );
    listed
        .get("RecoveryPoints")
        .and_then(Value::as_array)
        .and_then(|points| {
            points
                .iter()
                .find(|point| string_at(point, "RecoveryPointArn") == point_arn)
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the vault's listing must carry {point_arn}, got {listed}")
        })
}

/// Assert that every read of the backup that wrote `point_arn` reports `NO_SIZE`: the job and
/// the bytes it transferred, the account's job listing, the recovery point and the vault's
/// listing of it.
async fn assert_backup_reports_no_size(
    router: &MultiServiceRouter,
    case: &str,
    vault: &str,
    point_arn: &str,
    counted: &str,
) {
    let listed_job = job_of_point(router, case, point_arn).await;
    let job_id = string_at(&listed_job, "BackupJobId");
    let described_job = settled_backup_job(router, case, &job_id).await;
    assert_eq!(
        reported_size(case, "the backup job", &described_job),
        NO_SIZE,
        "case {case:?}: DescribeBackupJob must report {NO_SIZE} bytes backed up for this \
         resource type, not {counted}, got {described_job}"
    );
    assert_eq!(
        described_job.get("BytesTransferred").and_then(Value::as_i64),
        Some(NO_SIZE),
        "case {case:?}: a settled job has moved the whole backup, whose reported size is \
         {NO_SIZE}, not {counted}, got {described_job}"
    );
    assert_eq!(
        reported_size(case, "the account's job listing", &listed_job),
        NO_SIZE,
        "case {case:?}: ListBackupJobs must report the same {NO_SIZE}, got {listed_job}"
    );

    let (status, read) = call(
        router,
        "GET",
        &format!("/backup-vaults/{vault}/recovery-points/{point_arn}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeRecoveryPoint must answer 200, got {status} {read:?}"
    );
    assert_eq!(
        reported_size(case, "the recovery point", &read),
        NO_SIZE,
        "case {case:?}: the point the backup wrote must report {NO_SIZE}, not {counted}, got \
         {read}"
    );

    let listed = listed_point(router, case, vault, point_arn).await;
    assert_eq!(
        reported_size(case, "the vault's listing", &listed),
        NO_SIZE,
        "case {case:?}: the vault's listing must report the same {NO_SIZE}, got {listed}"
    );
}

/// RULE, over how much the protected resource holds: a backup of a plain Amazon DynamoDB
/// table reports no size at all, whatever that table holds and whatever its own service
/// reports about its stored bytes — so the figure is not counted over the backed-up data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backup_of_a_plain_dynamodb_table_reports_no_size_whatever_it_holds() {
    for (index, holding) in holdings().into_iter().enumerate() {
        let case = holding.case;
        let router = emulator();
        let vault = format!("reported-size-{index}");
        let table = format!("reported-size-table-{index}");
        let (status, created) = create_vault(&router, &vault, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault must succeed, got {status} {created:?}"
        );
        let table_arn = table_holding(&router, case, &table, &holding.items).await;
        // What the table's own service says about its stored bytes, and what this emulator
        // holds for it, are both quantities the reported size must NOT be taken from; the
        // figure appears in the failure messages so a regression names what it was taken from.
        let counted = format!(
            "the {} bytes DynamoDB reports stored for the table",
            table_size_bytes(&router, case, &table).await
        );

        let point_arn = completed_point(&router, &vault, &table_arn).await;
        assert_backup_reports_no_size(&router, case, &vault, &point_arn, &counted).await;

        // The figure was settled by the backup: a later write to the protected resource does
        // not move what the stored point reports.
        let (status, written) = dynamodb(
            &router,
            "PutItem",
            json!({
                "TableName": table,
                "Item": {"pk": {"S": "written-after-the-backup"}, "blob": {"S": "y".repeat(512)}},
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the later PutItem must succeed, got {status} {written:?}"
        );
        assert_backup_reports_no_size(&router, case, &vault, &point_arn, &counted).await;
    }
}

/// RULE, over the TYPE of the protected resource: each type the rule's catalog names reports
/// no size, so the figure follows the resource's type rather than the one resource the
/// recorded request named.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reported_size_follows_the_protected_resources_type() {
    for (resource_type, resource_arn) in typed_resources() {
        let case = resource_type;
        // Backup alone: a resource of these types belongs to a service this emulator is not
        // asked about here, so what is under test is the TYPE the ARN carries.
        let router = router();
        let vault = format!("typed-size-{}", resource_type.to_lowercase());
        let (status, created) = create_vault(&router, &vault, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault must succeed, got {status} {created:?}"
        );
        let (status, started) = start_job(
            &router,
            &json!({
                "BackupVaultName": vault,
                "ResourceArn": resource_arn,
                "IamRoleArn": backup_role(),
            })
            .to_string(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: StartBackupJob for {resource_arn} must succeed, got {status} \
             {started:?}"
        );
        let point_arn = string_at(&started, "RecoveryPointArn");
        let job_id = string_at(&started, "BackupJobId");
        let described_job = settled_backup_job(&router, case, &job_id).await;
        assert_eq!(
            string_at(&described_job, "ResourceType"),
            resource_type,
            "case {case:?}: the job must report the resource type read off {resource_arn}, \
             got {described_job}"
        );
        assert_backup_reports_no_size(
            &router,
            case,
            &vault,
            &point_arn,
            "a figure of this emulator's own",
        )
        .await;
    }
}
