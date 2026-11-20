// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two refusals a request's SECONDARY-INDEX DECLARATIONS earn
//! for their COUNT and for the key they lead with.
//!
//! The rules pinned here:
//!
//! 1. a list of index declarations longer than the per-table limit for that KIND of index
//!    is refused HTTP 400 `ValidationException` with the service's parameter-value clause
//!    over `Number of <the member that carried them> exceeds per-table limit of <limit>`:
//!    the member a caller has to shorten and the limit it broke, and nothing of how many
//!    it sent or which declaration was the one too many. The limits are the published
//!    per-table quotas — "You can define a maximum of 5 local secondary indexes per table"
//!    and 20 global secondary indexes ("Service, Account, and Table Quotas" in the Amazon
//!    DynamoDB Developer Guide) — and the rule holds wherever a request declares a table's
//!    indexes: `CreateTable`'s own two members and the index OVERRIDES of the two restores
//!    (`smithy_query(query="operation", operation="CreateTable,RestoreTableFromBackup,
//!    RestoreTableToPointInTime")` gives `GlobalSecondaryIndexes`/`LocalSecondaryIndexes`
//!    and `GlobalSecondaryIndexOverride`/`LocalSecondaryIndexOverride`, none of them
//!    carrying a `@length` the request validator could report instead);
//! 2. a local secondary index's "key schema must begin with the same partition key as the
//!    table" (`CreateTable$LocalSecondaryIndexes`), so one leading with another attribute
//!    is refused with that same clause over `Index KeySchema does not have the same
//!    leading hash key as table KeySchema for index: <IndexName>. index hash key:
//!    <the attribute the index led with>, table hash key: <the table's own>` — every
//!    variable part read off the request;
//! 3. a refused declaration stores nothing: no table is created under the name it named.
//!
//! Each rule is asserted over a TABLE of inputs rather than one example — the kind of
//! index, the member it was declared in, the operation that carried it, how far past the
//! limit the list went (and the list AT the limit, which is not this refusal), the index
//! name, and which attribute each of the two schemas leads with.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message};

/// The clause every refusal of a parameter value in this service opens with.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid";

/// The published per-table quota for local secondary indexes ("You can define a maximum of
/// 5 local secondary indexes per table").
const LOCAL_LIMIT: usize = 5;

/// The published per-table quota for global secondary indexes (20 per table).
const GLOBAL_LIMIT: usize = 20;

/// The whole message a list past the per-table limit for its kind of index earns.
fn exceeds_limit(member: &str, limit: usize) -> String {
    format!("{INVALID_PARAMETER}: Number of {member} exceeds per-table limit of {limit}")
}

/// The whole message an index that does not lead with the table's partition key earns.
fn wrong_leading_hash_key(index: &str, index_hash: &str, table_hash: &str) -> String {
    format!(
        "{INVALID_PARAMETER}: Index KeySchema does not have the same leading hash key as table \
         KeySchema for index: {index}. index hash key: {index_hash}, table hash key: {table_hash}"
    )
}

/// `count` local secondary index declarations over the table partition key `partition`,
/// each with a sort key of its own.
fn local_indexes(partition: &str, count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|which| {
                json!({
                    "IndexName": format!("lsi{which}"),
                    "KeySchema": [
                        {"AttributeName": partition, "KeyType": "HASH"},
                        {"AttributeName": format!("alt{which}"), "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                })
            })
            .collect(),
    )
}

/// `count` global secondary index declarations, each keyed on a spare attribute.
fn global_indexes(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|which| {
                json!({
                    "IndexName": format!("gsi{which}"),
                    "KeySchema": [{"AttributeName": format!("alt{which}"), "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                })
            })
            .collect(),
    )
}

/// The attribute definitions a table keyed on `pk`/`sk` needs for `spares` spare
/// attributes named `alt<n>`.
fn definitions(spares: usize) -> Value {
    let mut declared = vec![
        json!({"AttributeName": "pk", "AttributeType": "S"}),
        json!({"AttributeName": "sk", "AttributeType": "S"}),
    ];
    declared.extend((0..spares).map(|which| {
        json!({"AttributeName": format!("alt{which}"), "AttributeType": "S"})
    }));
    Value::Array(declared)
}

/// A `CreateTable` body for `name` keyed `pk` HASH / `sk` RANGE, with `spares` spare
/// attributes defined beside them.
fn create_table(name: &str, spares: usize) -> Value {
    json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"},
        ],
        "AttributeDefinitions": definitions(spares),
    })
}

/// A well-formed backup ARN of this endpoint that names no backup: the restore rows are
/// about the OVERRIDE LIST, which is read before any backup is looked up.
fn backup_arn() -> String {
    format!(
        "arn:aws:dynamodb:{}:000000000000:table/Source/backup/01234567890123-deadbeef",
        crate::emulator::REGION
    )
}

/// One row of the count table: the operation, the member the indexes were declared in, the
/// per-table limit for that kind, and the request body declaring `count` of them.
struct CountCase {
    case: &'static str,
    operation: &'static str,
    member: &'static str,
    limit: usize,
    body: fn(usize) -> Value,
}

/// Every member a table's secondary indexes are declared in, each with the per-table limit
/// for the kind of index it carries.
const COUNT_CASES: &[CountCase] = &[
    CountCase {
        case: "a create declaring local secondary indexes",
        operation: "CreateTable",
        member: "LocalSecondaryIndexes",
        limit: LOCAL_LIMIT,
        body: |count| {
            let mut request = create_table(&format!("LocalCount{count}"), count);
            request["LocalSecondaryIndexes"] = local_indexes("pk", count);
            request
        },
    },
    CountCase {
        case: "a create declaring global secondary indexes",
        operation: "CreateTable",
        member: "GlobalSecondaryIndexes",
        limit: GLOBAL_LIMIT,
        body: |count| {
            let mut request = create_table(&format!("GlobalCount{count}"), count);
            request["GlobalSecondaryIndexes"] = global_indexes(count);
            request
        },
    },
    CountCase {
        case: "a restore from a backup overriding its local secondary indexes",
        operation: "RestoreTableFromBackup",
        member: "LocalSecondaryIndexOverride",
        limit: LOCAL_LIMIT,
        body: |count| {
            json!({
                "TargetTableName": format!("FromBackupLocal{count}"),
                "BackupArn": backup_arn(),
                "LocalSecondaryIndexOverride": local_indexes("pk", count),
            })
        },
    },
    CountCase {
        case: "a restore from a backup overriding its global secondary indexes",
        operation: "RestoreTableFromBackup",
        member: "GlobalSecondaryIndexOverride",
        limit: GLOBAL_LIMIT,
        body: |count| {
            json!({
                "TargetTableName": format!("FromBackupGlobal{count}"),
                "BackupArn": backup_arn(),
                "GlobalSecondaryIndexOverride": global_indexes(count),
            })
        },
    },
    CountCase {
        case: "a point-in-time restore overriding its local secondary indexes",
        operation: "RestoreTableToPointInTime",
        member: "LocalSecondaryIndexOverride",
        limit: LOCAL_LIMIT,
        body: |count| {
            json!({
                "SourceTableName": "Source",
                "TargetTableName": format!("InTimeLocal{count}"),
                "UseLatestRestorableTime": true,
                "LocalSecondaryIndexOverride": local_indexes("pk", count),
            })
        },
    },
    CountCase {
        case: "a point-in-time restore overriding its global secondary indexes",
        operation: "RestoreTableToPointInTime",
        member: "GlobalSecondaryIndexOverride",
        limit: GLOBAL_LIMIT,
        body: |count| {
            json!({
                "SourceTableName": "Source",
                "TargetTableName": format!("InTimeGlobal{count}"),
                "UseLatestRestorableTime": true,
                "GlobalSecondaryIndexOverride": global_indexes(count),
            })
        },
    },
];

#[tokio::test]
async fn an_index_list_past_the_per_table_limit_names_the_member_and_that_limit() {
    let router = emulator();

    for row in COUNT_CASES {
        // One past the limit and half again as many: the sentence states the limit, so it
        // reads the same however far past the list went.
        for count in [row.limit + 1, row.limit + row.limit / 2 + 1] {
            let request = (row.body)(count);
            let (status, body) = call(&router, row.operation, request.clone()).await;
            let (code, stated) = (error_code(&body), message(&body));
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {:?} with {count} indexes: expected 400 ValidationException, got {status} \
                 {code} with {stated:?}",
                row.case
            );
            assert_eq!(
                stated,
                exceeds_limit(row.member, row.limit),
                "case {:?} with {count} indexes: expected the per-table limit message, got \
                 {stated:?}",
                row.case
            );
        }

        // AT the limit the request is not refused for its COUNT at all — whatever else it
        // answers — which is what makes the rows above about the bound and not an
        // off-by-one.
        let at_the_limit = (row.body)(row.limit);
        let (status, body) = call(&router, row.operation, at_the_limit).await;
        let stated = message(&body);
        assert!(
            !stated.contains("exceeds per-table limit"),
            "case {:?} with {} indexes: a list AT the limit is not past it, got {status} with \
             {stated:?}",
            row.case,
            row.limit
        );
    }
}

#[tokio::test]
async fn a_local_index_not_leading_with_the_table_partition_key_names_both_hash_keys() {
    let router = emulator();

    // Each row: the case, the table's own name, how many spare attributes it defines, the
    // declaration, and the whole message. The rows differ along
    // which attribute each schema leads with, the index name, and whether the offending
    // index is the only one declared or follows a well-formed one.
    let rows: Vec<(&str, &str, usize, Value, String)> = vec![
        (
            "a local index leading with a spare attribute",
            "KeyOther",
            1,
            json!([{
                "IndexName": "lsi1",
                "KeySchema": [
                    {"AttributeName": "alt0", "KeyType": "HASH"},
                    {"AttributeName": "sk", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "ALL"},
            }]),
            wrong_leading_hash_key("lsi1", "alt0", "pk"),
        ),
        (
            "a local index leading with the table's own SORT key",
            "KeySort",
            1,
            json!([{
                "IndexName": "bySort",
                "KeySchema": [
                    {"AttributeName": "sk", "KeyType": "HASH"},
                    {"AttributeName": "alt0", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }]),
            wrong_leading_hash_key("bySort", "sk", "pk"),
        ),
        (
            "the SECOND of two local indexes leading with another attribute",
            "KeySecond",
            2,
            json!([
                {
                    "IndexName": "lsi0",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "alt0", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": "lsi1",
                    "KeySchema": [
                        {"AttributeName": "alt1", "KeyType": "HASH"},
                        {"AttributeName": "alt0", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ]),
            wrong_leading_hash_key("lsi1", "alt1", "pk"),
        ),
    ];

    for (case, name, spares, indexes, expected) in rows {
        let mut request = create_table(name, spares);
        request["LocalSecondaryIndexes"] = indexes;
        let (status, body) = call(&router, "CreateTable", request).await;
        let (code, stated) = (error_code(&body), message(&body));
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code} with {stated:?}"
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: expected AWS's leading-hash-key message, got {stated:?}"
        );

        // Nothing was stored: the name the refused create named is still free, so a
        // DescribeTable of it is the lookup miss.
        let (described, missed) = call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (described, error_code(&missed).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: a refused create stores no table, got {described} with {missed}"
        );
    }

    // The table whose partition key the index DOES lead with is created: the rule is about
    // the leading key and not about declaring a local index at all.
    let mut accepted = create_table("KeyLed", 1);
    accepted["LocalSecondaryIndexes"] = local_indexes("pk", 1);
    let (status, body) = call(&router, "CreateTable", accepted).await;
    assert_eq!(
        status, 200,
        "case a local index leading with the table's partition key: expected it created, got \
         {status} with {body}"
    );
}
