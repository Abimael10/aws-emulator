// SPDX-License-Identifier: CC0-1.0

//! What a restore of a DynamoDB table recovery point RE-CREATES: the shape the
//! protected resource had, and the data it held.
//!
//! The rules under test:
//!
//! * a restore re-creates the protected resource with the SHAPE the recovery point
//!   was taken of — every structural member of the source table's description that
//!   `CreateTable` takes, which includes its secondary indexes, global and local
//!   alike, with their own key schemas and projections — so the restored table
//!   offers the query paths the source offered and the restore does not end
//!   `FAILED` on the attribute definitions those indexes are declared over;
//! * a restore puts the DATA back: the items the recovery point captured when the
//!   backup was taken are what the restored table answers reads for, whole and
//!   with every attribute they carried, and a backup's reported size is the size
//!   of what it captured.
//!
//! The rows differ along what the rules generalise over: which index kinds the
//! source table carries (none, global, local, both), the projection each index
//! declares, the billing mode the table is in (so an index of a provisioned table
//! is created with capacity of its own and one of a pay-per-request table with
//! none), the attribute types the captured items carry, and how many items there
//! are — including none at all.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! behaviour a `backup` and a `dynamodb` SDK client see. The harness is the shared
//! one in `vault_and_backup_job.rs`, the Backup-and-DynamoDB emulator and the
//! DynamoDB wire are `restore_job.rs`'s, and the settled-point setup is
//! `recovery_point_deletion.rs`'s.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::recovery_point_deletion::completed_point;
use crate::restore_job::{dynamodb, emulator, settled_restore};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, string_at, ACCOUNT, POLL_DEADLINE, POLL_WAIT,
};

/// The vault every backup of this suite is taken into.
const VAULT: &str = "restore-contents";

/// The table every row backs up, and the table its restore creates.
const SOURCE_TABLE: &str = "orders";
const RESTORED_TABLE: &str = "orders-restored";

/// `TableStatus` / `IndexStatus` of a table and an index that are usable (model enums
/// `TableStatus`, `IndexStatus`).
const ACTIVE: &str = "ACTIVE";

/// `RestoreJobStatus` a restore that did its work ends in.
const COMPLETED: &str = "COMPLETED";

/// The role every backup and restore of this suite runs as.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// One source table, and what its restore has to reproduce.
struct Case {
    name: &'static str,
    /// Whether the source table is provisioned (so its indexes carry capacity) or
    /// billed per request (so they carry none).
    provisioned: bool,
    key_schema: Value,
    attribute_definitions: Value,
    /// The `GlobalSecondaryIndexes` / `LocalSecondaryIndexes` the source table is
    /// created with, each an empty array for a table that carries none of that kind.
    global_indexes: Value,
    local_indexes: Value,
    /// The items put into the source table before the backup — the data the restore
    /// has to put back.
    items: Vec<Value>,
    /// An index of the restored table to query, the attribute to query it by, the
    /// value to query for, and the `pk` of every item that query must answer with.
    query: Option<(&'static str, &'static str, Value, Vec<&'static str>)>,
}

/// The rows: see the module docs for the dimensions they span.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a pay-per-request table with a global secondary index projecting ALL",
            provisioned: false,
            key_schema: json!([{"AttributeName": "pk", "KeyType": "HASH"}]),
            attribute_definitions: json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
            ]),
            global_indexes: json!([{
                "IndexName": "by-owner",
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }]),
            local_indexes: json!([]),
            items: vec![
                json!({"pk": {"S": "order-1"}, "owner": {"S": "alice"}, "total": {"N": "42"}}),
                json!({"pk": {"S": "order-2"}, "owner": {"S": "bob"}, "total": {"N": "7"}}),
            ],
            query: Some((
                "by-owner",
                "owner",
                json!({"S": "alice"}),
                vec!["order-1"],
            )),
        },
        Case {
            name: "a provisioned table with a global secondary index of its own capacity",
            provisioned: true,
            key_schema: json!([{"AttributeName": "pk", "KeyType": "HASH"}]),
            attribute_definitions: json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "region", "AttributeType": "S"},
            ]),
            global_indexes: json!([{
                "IndexName": "by-region",
                "KeySchema": [{"AttributeName": "region", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 3},
            }]),
            local_indexes: json!([]),
            items: vec![
                json!({"pk": {"S": "order-1"}, "region": {"S": "eu"}, "total": {"N": "11"}}),
                json!({"pk": {"S": "order-2"}, "region": {"S": "eu"}, "total": {"N": "12"}}),
                json!({"pk": {"S": "order-3"}, "region": {"S": "us"}, "total": {"N": "13"}}),
            ],
            query: Some((
                "by-region",
                "region",
                json!({"S": "eu"}),
                vec!["order-1", "order-2"],
            )),
        },
        Case {
            name: "a table with a local secondary index projecting INCLUDE",
            provisioned: false,
            key_schema: json!([
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ]),
            attribute_definitions: json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "total", "AttributeType": "N"},
            ]),
            global_indexes: json!([]),
            local_indexes: json!([{
                "IndexName": "by-total",
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "total", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "INCLUDE", "NonKeyAttributes": ["owner"]},
            }]),
            items: vec![
                json!({
                    "pk": {"S": "customer-1"},
                    "sk": {"N": "1"},
                    "total": {"N": "42"},
                    "owner": {"S": "alice"},
                }),
                json!({
                    "pk": {"S": "customer-1"},
                    "sk": {"N": "2"},
                    "total": {"N": "43"},
                    "owner": {"S": "bob"},
                }),
            ],
            query: Some((
                "by-total",
                "pk",
                json!({"S": "customer-1"}),
                vec!["customer-1", "customer-1"],
            )),
        },
        Case {
            name: "a table carrying both index kinds, and items of every attribute type",
            provisioned: false,
            key_schema: json!([
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ]),
            attribute_definitions: json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "owner", "AttributeType": "S"},
                {"AttributeName": "total", "AttributeType": "N"},
            ]),
            global_indexes: json!([{
                "IndexName": "by-owner",
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }]),
            local_indexes: json!([{
                "IndexName": "by-total",
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "total", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }]),
            items: vec![json!({
                "pk": {"S": "order-1"},
                "sk": {"N": "1"},
                "owner": {"S": "alice"},
                "total": {"N": "42"},
                "paid": {"BOOL": true},
                "labels": {"SS": ["red", "blue"]},
                "counts": {"NS": ["1", "2"]},
                "lines": {"L": [{"N": "1"}, {"S": "two"}]},
                "meta": {"M": {"note": {"S": "kept"}}},
                "receipt": {"B": "aGVsbG8="},
                "cancelled": {"NULL": true},
            })],
            query: Some((
                "by-owner",
                "owner",
                json!({"S": "alice"}),
                vec!["order-1"],
            )),
        },
        Case {
            name: "a table with an index and no items at all",
            provisioned: false,
            key_schema: json!([{"AttributeName": "pk", "KeyType": "HASH"}]),
            attribute_definitions: json!([
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
            ]),
            global_indexes: json!([{
                "IndexName": "by-owner",
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }]),
            local_indexes: json!([]),
            items: Vec::new(),
            query: Some(("by-owner", "owner", json!({"S": "alice"}), Vec::new())),
        },
    ]
}

/// Create the source table of `case` through DynamoDB's own wire, put its items into
/// it, and answer the ARN that service gave it.
async fn source_table(router: &MultiServiceRouter, case: &Case) -> String {
    let mut body = json!({
        "TableName": SOURCE_TABLE,
        "KeySchema": case.key_schema,
        "AttributeDefinitions": case.attribute_definitions,
    });
    if case.provisioned {
        body["ProvisionedThroughput"] = json!({"ReadCapacityUnits": 5, "WriteCapacityUnits": 6});
    } else {
        body["BillingMode"] = json!("PAY_PER_REQUEST");
    }
    if !case
        .global_indexes
        .as_array()
        .is_some_and(|indexes| indexes.is_empty())
    {
        body["GlobalSecondaryIndexes"] = case.global_indexes.clone();
    }
    if !case
        .local_indexes
        .as_array()
        .is_some_and(|indexes| indexes.is_empty())
    {
        body["LocalSecondaryIndexes"] = case.local_indexes.clone();
    }
    let (status, created) = dynamodb(router, "CreateTable", body).await;
    assert_eq!(
        status, 200,
        "case {:?}: setup CreateTable must succeed, got {status} {created:?}",
        case.name
    );
    let arn = string_at(
        created.get("TableDescription").unwrap_or(&Value::Null),
        "TableArn",
    );
    // The table is usable once its own service reports it ACTIVE, which is the
    // condition a client waits for before writing to it.
    active_table(router, case.name, SOURCE_TABLE).await;
    for item in &case.items {
        let (status, written) = dynamodb(
            router,
            "PutItem",
            json!({"TableName": SOURCE_TABLE, "Item": item}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: setup PutItem {item:?} must succeed, got {status} {written:?}",
            case.name
        );
    }
    arn
}

/// The description DynamoDB gives of `table`, once it reports the table and every
/// index on it `ACTIVE`.
///
/// The wait is on the CONDITION, with room well past the span that service's clock
/// takes to create a table, so a case never turns on how fast the host runs.
async fn active_table(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, described) = dynamodb(router, "DescribeTable", json!({"TableName": table})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable {table} must answer 200, got {status} {described:?}"
        );
        let described = described.get("Table").cloned().unwrap_or(Value::Null);
        // A global secondary index reports a status of its own; a local one has none
        // to report (the model's `LocalSecondaryIndexDescription` carries no
        // `IndexStatus`), so only the indexes that report one are waited for.
        let indexes_active = ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"]
            .iter()
            .flat_map(|member| described.get(*member).and_then(Value::as_array).cloned())
            .flatten()
            .filter(|index| index.get("IndexStatus").is_some())
            .all(|index| string_at(&index, "IndexStatus") == ACTIVE);
        if string_at(&described, "TableStatus") == ACTIVE && indexes_active {
            return described;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {table} and its indexes must become {ACTIVE} within \
             {POLL_DEADLINE:?}, got {described:?}"
        );
        ::tokio::time::sleep(POLL_WAIT).await;
    }
}

/// The index descriptions of `described` under `member`, reduced to what a restore
/// has to reproduce: the index name, its key schema and its projection.
fn index_shapes(described: &Value, member: &str) -> Vec<Value> {
    described
        .get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|index| {
            let mut shape = Map::new();
            for member in ["IndexName", "KeySchema", "Projection"] {
                shape.insert(
                    member.to_string(),
                    index.get(member).cloned().unwrap_or(Value::Null),
                );
            }
            Value::Object(shape)
        })
        .collect()
}

/// One item as a reading two scans of the same contents compare equal by: its
/// attributes in name order, each with the value it carries. The wire form of an item
/// is a JSON object, whose member ORDER says nothing about the item.
fn canonical(item: &Value) -> String {
    let mut attributes: Vec<String> = item
        .as_object()
        .map(|members| {
            members
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect()
        })
        .unwrap_or_default();
    attributes.sort();
    attributes.join(",")
}

/// Every item `table` holds, read through DynamoDB's own `Scan`, in the canonical
/// reading above and sorted, so two readings of the same contents compare equal.
async fn scanned_items(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<String> {
    let (status, scanned) = dynamodb(router, "Scan", json!({"TableName": table})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Scan {table} must answer 200, got {status} {scanned:?}"
    );
    let mut items: Vec<String> = scanned
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(canonical)
        .collect();
    items.sort();
    items
}

/// Start the restore of `recovery_point_arn` into [`RESTORED_TABLE`] and answer the
/// id of the job it starts.
async fn start_restore(router: &MultiServiceRouter, case: &str, recovery_point_arn: &str) -> String {
    let body = json!({
        "RecoveryPointArn": recovery_point_arn,
        "Metadata": {"targetTableName": RESTORED_TABLE},
        "IamRoleArn": role_arn(),
        "ResourceType": "DynamoDB",
    });
    let (status, started) = call(router, "PUT", "/restore-jobs", Some(&body.to_string())).await;
    assert_eq!(
        (status, error_code(&started).as_str()),
        (200, "<none>"),
        "case {case:?}: expected StartRestoreJob 200, got {status} {started:?}"
    );
    string_at(&started, "RestoreJobId")
}

/// A restore re-creates the source table's secondary indexes and puts its items
/// back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_reproduces_the_source_tables_indexes_and_items() {
    for case in cases() {
        let name = case.name;
        let router = emulator();
        let (status, created) = create_vault(&router, VAULT, "{}").await;
        assert_eq!(
            status, 200,
            "case {name:?}: setup CreateBackupVault must succeed, got {status} {created:?}"
        );
        let table_arn = source_table(&router, &case).await;
        let source = active_table(&router, name, SOURCE_TABLE).await;
        let source_items = scanned_items(&router, name, SOURCE_TABLE).await;

        let point = completed_point(&router, VAULT, &table_arn).await;
        // The backup's reported size is the size the table's OWN service meters for it,
        // which it refreshes on its own schedule — so a table written to an instant ago is
        // backed up at the size that service still reports, whatever the backup copied. What
        // proves the data travelled is the restore below, not this figure.
        let (status, read) = call(
            &router,
            "GET",
            &format!("/backup-vaults/{VAULT}/recovery-points/{point}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: DescribeRecoveryPoint must answer 200, got {status} {read:?}"
        );
        let metered_table_size = source
            .get("TableSizeBytes")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let captured_size = read
            .get("BackupSizeInBytes")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        assert_eq!(
            captured_size, metered_table_size,
            "case {name:?}: the backup of {} item(s) must report the size the table's own \
             service meters ({metered_table_size}), got {captured_size}",
            case.items.len()
        );

        let job_id = start_restore(&router, name, &point).await;
        let ended = settled_restore(&router, &job_id).await;
        assert_eq!(
            string_at(&ended, "Status"),
            COMPLETED,
            "case {name:?}: the restore must complete, got {ended:?}"
        );
        // What the restore moved is the backup it read, so the job reports that
        // backup's own size rather than none.
        assert_eq!(
            ended.get("BackupSizeInBytes").and_then(Value::as_i64),
            Some(captured_size),
            "case {name:?}: the restore must report the size of the backup it read, got \
             {ended:?}"
        );

        // THE SHAPE: the restored table carries the source's indexes, each with the
        // source's key schema and projection, and every one of them is usable.
        let restored = active_table(&router, name, RESTORED_TABLE).await;
        for member in ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"] {
            assert_eq!(
                index_shapes(&restored, member),
                index_shapes(&source, member),
                "case {name:?}: the restored table's {member} must be the source's, got \
                 {restored:?}"
            );
        }

        // THE DATA: every item the recovery point captured is in the restored table,
        // with every attribute it carried.
        let restored_items = scanned_items(&router, name, RESTORED_TABLE).await;
        assert_eq!(
            restored_items.len(),
            case.items.len(),
            "case {name:?}: the restored table must hold the backed-up items, got \
             {restored_items:?}"
        );
        assert_eq!(
            restored_items, source_items,
            "case {name:?}: the restored table's items must be the source's, got \
             {restored_items:?}"
        );

        // And they are reachable through the restored index, which is what an
        // index-less restore would have lost.
        if let Some((index, attribute, value, expected)) = case.query {
            let (status, answered) = dynamodb(
                &router,
                "Query",
                json!({
                    "TableName": RESTORED_TABLE,
                    "IndexName": index,
                    "KeyConditionExpression": "#a = :a",
                    "ExpressionAttributeNames": {"#a": attribute},
                    "ExpressionAttributeValues": {":a": value},
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name:?}: Query on the restored {index} must answer 200, got {status} \
                 {answered:?}"
            );
            let answered_keys: Vec<String> = {
                let mut keys: Vec<String> = answered
                    .get("Items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|item| string_at(item.get("pk").unwrap_or(&Value::Null), "S"))
                    .collect();
                // The rule under test is WHICH items the restored index answers for,
                // not the order a query reports them in.
                keys.sort();
                keys
            };
            let mut expected_keys: Vec<String> =
                expected.iter().map(ToString::to_string).collect();
            expected_keys.sort();
            assert_eq!(
                answered_keys, expected_keys,
                "case {name:?}: the restored {index} must answer for the backed-up items, got \
                 {answered:?}"
            );
        }
    }
}
