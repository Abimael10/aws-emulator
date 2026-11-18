// SPDX-License-Identifier: CC0-1.0

//! What a global secondary index LOOKS LIKE to a reader, which is what another service
//! resolves an `index` scalable target against.
//!
//! `regions_indexes_and_capacity.rs` covers `CreateTable`'s index rules and `UpdateTable`'s
//! capacity moves. This suite pins the narrow contract a CROSS-SERVICE reader depends on —
//! Application Auto Scaling resolves a `dynamodb:index:*` target by looking the named index
//! up on the table and reading its provisioned capacity — over a table of index shapes:
//!
//! * every index the table was created with is reported by `DescribeTable`, by name, with
//!   the key schema and the provisioned capacity it was created with;
//! * an index name the table does not carry is absent from that list, so a reader cannot
//!   resolve one that does not exist;
//! * and an `UpdateTable` entry that asks for lifecycle work on an index the table does
//!   not carry is refused as the request defect it is, naming the action and the index,
//!   while the entry naming an index the table does carry is applied.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message};

/// One row: the index to create, and the capacity a reader must find on it.
struct IndexRow {
    case: &'static str,
    index: &'static str,
    key: &'static str,
    read: i64,
    write: i64,
}

/// The rows differ in the attribute the index is keyed on, in its name, and in both
/// capacity dimensions — the values a `dynamodb:index:ReadCapacityUnits` or
/// `…:WriteCapacityUnits` target scales.
const INDEX_TABLE: &[IndexRow] = &[
    IndexRow {
        case: "an index keyed on a string attribute",
        index: "by-customer",
        key: "customer",
        read: 3,
        write: 4,
    },
    IndexRow {
        case: "an index keyed on a number attribute, with equal capacities",
        index: "by-placed",
        key: "placed",
        read: 1,
        write: 1,
    },
];

#[tokio::test]
async fn every_created_index_is_reported_with_the_capacity_it_was_created_with() {
    let router = emulator();
    let indexes: Vec<Value> = INDEX_TABLE
        .iter()
        .map(|row| {
            json!({
                "IndexName": row.index,
                "KeySchema": [{"AttributeName": row.key, "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": row.read, "WriteCapacityUnits": row.write,
                },
            })
        })
        .collect();
    let (status, created) = call(
        &router,
        "CreateTable",
        json!({
            "TableName": "orders",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "customer", "AttributeType": "S"},
                {"AttributeName": "placed", "AttributeType": "N"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 2},
            "GlobalSecondaryIndexes": indexes,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"create\": expected CreateTable to answer 200, got {status} {created}"
    );

    let (status, described) = call(&router, "DescribeTable", json!({"TableName": "orders"})).await;
    assert_eq!(
        status, 200,
        "case \"describe\": expected DescribeTable to answer 200, got {status} {described}"
    );
    let reported = described["Table"]["GlobalSecondaryIndexes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        reported.len(),
        INDEX_TABLE.len(),
        "case \"describe\": expected every index the table was created with, got {described}"
    );

    for row in INDEX_TABLE {
        let case = row.case;
        let index = reported
            .iter()
            .find(|index| index["IndexName"] == json!(row.index))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected an index named {:?}, got {reported:?}", row.index)
            });
        assert_eq!(
            index["KeySchema"],
            json!([{"AttributeName": row.key, "KeyType": "HASH"}]),
            "case {case:?}: expected the key schema it was created with, got {index}"
        );
        assert_eq!(
            index["ProvisionedThroughput"]["ReadCapacityUnits"],
            json!(row.read),
            "case {case:?}: expected the read capacity a reader scales, got {index}"
        );
        assert_eq!(
            index["ProvisionedThroughput"]["WriteCapacityUnits"],
            json!(row.write),
            "case {case:?}: expected the write capacity a reader scales, got {index}"
        );
    }

    // An index the table does not carry is absent, so a reader resolving one by name
    // finds nothing rather than a default.
    assert!(
        !reported
            .iter()
            .any(|index| index["IndexName"] == json!("by-nothing")),
        "case \"an index the table does not carry\": expected it to be absent, got {reported:?}"
    );
}

#[tokio::test]
async fn a_table_created_without_an_index_reports_none() {
    let router = emulator();
    let (status, created) = call(
        &router,
        "CreateTable",
        json!({
            "TableName": "plain",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"create\": expected CreateTable to answer 200, got {status} {created}"
    );
    let (status, described) = call(&router, "DescribeTable", json!({"TableName": "plain"})).await;
    assert_eq!(
        status, 200,
        "case \"describe\": expected DescribeTable to answer 200, got {status} {described}"
    );
    assert!(
        described["Table"]
            .get("GlobalSecondaryIndexes")
            .is_none_or(|indexes| indexes.as_array().is_some_and(Vec::is_empty)),
        "case \"describe\": expected a table with no index to report none, got {described}"
    );
}

/// The rows an index-lifecycle entry naming no GLOBAL secondary index of the table is
/// asserted over: how the name misses the names the table does carry.
///
/// They differ along what the rule generalises over — a name nothing carries, the name of
/// an index of ANOTHER kind on this very table (a local secondary index is not one
/// `GlobalSecondaryIndexUpdates` names), the stored name in another case, and the stored
/// name with one character added. The local secondary index is the one row that IS on the
/// table, so a `Delete` of it breaks another rule than the lookup — see
/// [`NOT_A_GLOBAL_SECONDARY_INDEX`].
const ABSENT_INDEX_NAMES: &[(&str, &str)] = &[
    ("a name no index of the table carries", "by-nothing"),
    ("the name of a local secondary index", "by-rank"),
    ("the stored name in another case", "BY-OWNER"),
    ("the stored name with a trailing character", "by-ownerr"),
];

/// The name of the row above that is an index of the table but not a global secondary one.
const NOT_A_GLOBAL_SECONDARY_INDEX: &str = "by-rank";

#[tokio::test]
async fn an_index_lifecycle_entry_naming_no_index_of_the_table_is_refused_by_its_action() {
    let router = emulator();
    let table = "index_lifecycle_refusals";
    let (status, created) = call(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
                {"AttributeName": "rank", "AttributeType": "N"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 2, "WriteCapacityUnits": 2},
            "GlobalSecondaryIndexes": [{
                "IndexName": "by-owner",
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 3, "WriteCapacityUnits": 4},
            }],
            "LocalSecondaryIndexes": [{
                "IndexName": "by-rank",
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "rank", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"create\": expected CreateTable to answer 200, got {status} {created}"
    );
    describe_when_active(&router, "create", table).await;

    // The two actions that NAME an index, each refused for the index it could not find.
    type Action = (&'static str, fn(&str) -> Value);
    let actions: [Action; 2] = [
        ("Delete", |index| {
            json!({"Delete": {"IndexName": index}})
        }),
        ("Update", |index| {
            json!({"Update": {
                "IndexName": index,
                "ProvisionedThroughput": {"ReadCapacityUnits": 9, "WriteCapacityUnits": 9},
            }})
        }),
    ];

    for (action, build) in actions {
        for (row, absent) in ABSENT_INDEX_NAMES {
            let case = format!("a {action} of {absent:?} ({row})");
            let (status, answered) = call(
                &router,
                "UpdateTable",
                json!({
                    "TableName": table,
                    "GlobalSecondaryIndexUpdates": [build(absent)],
                }),
            )
            .await;
            // A `Delete` of an index that IS on the table but is a LOCAL secondary one
            // breaks the rule that this member acts on global secondary indexes only; every
            // other row names no index of the table at all, which is the lookup miss.
            let expected = if action == "Delete" && *absent == NOT_A_GLOBAL_SECONDARY_INDEX {
                (
                    "ValidationException".to_string(),
                    format!("Cannot delete an index that is not a GlobalSecondaryIndex: {absent}"),
                )
            } else {
                (
                    "ResourceNotFoundException".to_string(),
                    format!("Requested resource not found: Index {absent} for table {table}"),
                )
            };
            assert_eq!(
                (status, error_code(&answered), message(&answered)),
                (400, expected.0.clone(), expected.1.clone()),
                "case {case:?}: expected {expected:?}, got {status} with {answered}"
            );
        }
    }

    // None of those refusals wrote anything: the table is still ACTIVE and still carries
    // the index it was created with, at the capacity it was created with.
    let described = describe_when_active(&router, "after the refusals", table).await;
    assert_eq!(
        described["GlobalSecondaryIndexes"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "case \"after the refusals\": expected the created index to be untouched, got \
         {described}"
    );
    assert_eq!(
        described["GlobalSecondaryIndexes"][0]["ProvisionedThroughput"]["ReadCapacityUnits"],
        json!(3),
        "case \"after the refusals\": expected the created capacity to be untouched, got \
         {described}"
    );

    // The control: the same entry naming the index the table DOES carry is applied, and
    // the index is gone once the table settles.
    let (status, accepted) = call(
        &router,
        "UpdateTable",
        json!({
            "TableName": table,
            "GlobalSecondaryIndexUpdates": [{"Delete": {"IndexName": "by-owner"}}],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a Delete of the stored index\": expected 200, got {status} {accepted}"
    );
    let described = describe_when_active(&router, "after the delete", table).await;
    assert!(
        described
            .get("GlobalSecondaryIndexes")
            .is_none_or(|indexes| indexes.as_array().is_some_and(Vec::is_empty)),
        "case \"after the delete\": expected the index to be gone, got {described}"
    );
}
