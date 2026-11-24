// SPDX-License-Identifier: CC0-1.0

//! What a backup reports as the SIZE of the backup it took.
//!
//! The rule under test:
//!
//! > `BackupJob$BackupSizeInBytes` and `RecoveryPointByBackupVault$BackupSizeInBytes` are
//! > "the size, in bytes, of a backup", which is the size of the PROTECTED RESOURCE the
//! > backup was taken of as the service that OWNS it meters it — not a count of the bytes
//! > the backup copied. So the figure a backup reports is the one that service answers for
//! > the resource at the instant the backup is taken (for a DynamoDB table,
//! > `TableDescription$TableSizeBytes`, which "DynamoDB updates approximately every six
//! > hours", so a table written to an instant before the backup is metered at whatever its
//! > own service still reports), it is stored once on the recovery point, and every read of
//! > that backup — the job, the point and the vault's listing — reports that one value. A
//! > settled job has moved the whole backup, so its `BytesTransferred` is that same figure.
//!
//! The rows differ along what the rule generalises over: how much data the protected
//! resource holds when the backup is taken (none, a few small items, one item of a few
//! kilobytes) and whether the owning service has METERED that data (a table written through
//! its own wire, whose metered figure its six-hour schedule has not moved yet, and a table
//! DynamoDB's own restore created already holding its items, whose figures are metered when
//! it is created) — so no row can pass on a figure this emulator counted for itself, and no
//! row can pass on a constant.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire behaviour
//! a `backup` and a `dynamodb` SDK client see. The Backup-and-DynamoDB emulator and the
//! DynamoDB wire are [`crate::restore_job`]'s, the settled-point setup is
//! [`crate::recovery_point_deletion`]'s, and the vault calls are
//! [`crate::vault_and_backup_job`]'s; this suite adds no second copy of any of them.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::recovery_point_deletion::completed_point;
use crate::restore_job::{dynamodb, emulator};
use crate::vault_and_backup_job::{call, create_vault, error_code, string_at};

/// `TableStatus` of a table that is usable (model enum `TableStatus`).
const ACTIVE: &str = "ACTIVE";

/// How long a wait for one of those conditions runs in all, and how long each look waits
/// before the next: far past the span the emulator's own clock owes a table or a job, so a
/// case never turns on how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_WAIT: Duration = Duration::from_millis(100);

/// One row: what the protected resource holds when the backup is taken.
struct Row {
    case: &'static str,
    /// The items put into the table before the backup, as `PutItem` takes them.
    items: Vec<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a table holding nothing",
            items: Vec::new(),
        },
        Row {
            case: "a table holding a few small items",
            items: vec![
                json!({"pk": {"S": "order-1"}, "total": {"N": "42"}}),
                json!({"pk": {"S": "order-2"}, "total": {"N": "7"}}),
                json!({"pk": {"S": "order-3"}, "owner": {"S": "alice"}}),
            ],
        },
        Row {
            case: "a table holding one item of a few kilobytes",
            items: vec![json!({
                "pk": {"S": "order-large"},
                "blob": {"S": "x".repeat(4_096)},
            })],
        },
    ]
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

/// The size the owning service meters for the table `name`: `TableSizeBytes` as DynamoDB
/// itself reports it.
async fn metered_size(router: &MultiServiceRouter, case: &str, name: &str) -> i64 {
    let table = active_table(router, case, name).await;
    table
        .get("TableSizeBytes")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: DynamoDB must report a TableSizeBytes for {name}, got {table}")
        })
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

/// RULE: the size a backup reports is the size the owning service meters for the resource,
/// and every read of that backup reports that one figure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backup_reports_the_size_the_owning_service_meters_for_the_resource() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let vault = format!("metered-size-{index}");
        let table = format!("metered-table-{index}");
        let (status, created) = create_vault(&router, &vault, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault must succeed, got {status} {created:?}"
        );
        let table_arn = table_holding(&router, case, &table, &row.items).await;
        let metered = metered_size(&router, case, &table).await;

        let point_arn = completed_point(&router, &vault, &table_arn).await;
        let (status, read) = call(
            &router,
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
            metered,
            "case {case:?}: the point must report the size DynamoDB meters for the table \
             ({metered}), got {read}"
        );

        let job = job_of_point(&router, case, &point_arn).await;
        assert_eq!(
            reported_size(case, "the backup job", &job),
            metered,
            "case {case:?}: the job must report the same figure ({metered}), got {job}"
        );
        assert_eq!(
            job.get("BytesTransferred").and_then(Value::as_i64),
            Some(metered),
            "case {case:?}: a settled job has moved the whole backup, so it reports \
             {metered} bytes transferred, got {job}"
        );

        let listed = listed_point(&router, case, &vault, &point_arn).await;
        assert_eq!(
            reported_size(case, "the vault's listing", &listed),
            metered,
            "case {case:?}: the vault's listing must report the same figure ({metered}), got \
             {listed}"
        );

        // The figure was settled by the backup: a later write to the protected resource
        // does not move what the stored point reports.
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
        let (status, reread) = call(
            &router,
            "GET",
            &format!("/backup-vaults/{vault}/recovery-points/{point_arn}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the re-read must answer 200, got {status} {reread:?}"
        );
        assert_eq!(
            reported_size(case, "the re-read point", &reread),
            metered,
            "case {case:?}: a write after the backup leaves the stored figure alone, got \
             {reread}"
        );
    }
}

/// RULE, on a resource the owning service HAS metered: the backup reports that service's
/// figure, which is then not none — so the figure is read from the owning service rather
/// than counted here or fixed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backup_of_a_metered_resource_reports_that_services_figure() {
    let case = "a table DynamoDB restored from its own backup, already holding its items";
    let router = emulator();
    let vault = "metered-size-restored";
    let (status, created) = create_vault(&router, vault, "{}").await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateBackupVault must succeed, got {status} {created:?}"
    );
    let source = "metered-source";
    let restored = "metered-restored";
    table_holding(
        &router,
        case,
        source,
        &[
            json!({"pk": {"S": "order-1"}, "blob": {"S": "x".repeat(1_024)}}),
            json!({"pk": {"S": "order-2"}, "blob": {"S": "x".repeat(1_024)}}),
        ],
    )
    .await;

    // DynamoDB's own restore creates the table ALREADY holding the data, so that service
    // meters it then and there rather than on its refresh schedule.
    let (status, backed_up) = dynamodb(
        &router,
        "CreateBackup",
        json!({"TableName": source, "BackupName": "metered-source-backup"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateBackup must succeed, got {status} {backed_up:?}"
    );
    let backup_arn = string_at(
        backed_up
            .get("BackupDetails")
            .unwrap_or(&Value::Null),
        "BackupArn",
    );
    // The restore is asked for until that service accepts it: a backup of its own is
    // CREATING for a span before it can be restored from.
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, answer) = dynamodb(
            &router,
            "RestoreTableFromBackup",
            json!({"BackupArn": backup_arn, "TargetTableName": restored}),
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: RestoreTableFromBackup must be accepted within {POLL_DEADLINE:?}, \
             got {status} {answer:?}"
        );
        ::tokio::time::sleep(POLL_WAIT).await;
    }

    let restored_table = active_table(&router, case, restored).await;
    let metered = metered_size(&router, case, restored).await;
    assert!(
        metered > 0,
        "case {case:?}: DynamoDB must meter the restored table's own data, got {metered}"
    );

    // The backup of THAT table reports the figure its own service meters.
    let restored_arn = string_at(&restored_table, "TableArn");
    let restored_point = completed_point(&router, vault, &restored_arn).await;
    let (status, read) = call(
        &router,
        "GET",
        &format!("/backup-vaults/{vault}/recovery-points/{restored_point}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeRecoveryPoint must answer 200, got {status} {read:?}"
    );
    assert_eq!(
        reported_size(case, "the recovery point", &read),
        metered,
        "case {case:?}: the backup must report the {metered} bytes DynamoDB meters for the \
         restored table, got {read}"
    );
    let job = job_of_point(&router, case, &restored_point).await;
    assert_eq!(
        reported_size(case, "the backup job", &job),
        metered,
        "case {case:?}: the job must report the same figure ({metered}), got {job}"
    );
    assert_eq!(
        job.get("BytesTransferred").and_then(Value::as_i64),
        Some(metered),
        "case {case:?}: a settled job has moved the whole backup, so it reports {metered} \
         bytes transferred, got {job}"
    );
}
