// SPDX-License-Identifier: CC0-1.0

//! The per-table PROVISIONED-THROUGHPUT quota: what a request asking for more capacity than
//! one table may hold is answered, driven end to end over the wire.
//!
//! THE RULE: the provisioned capacity a request asks for one table — the table's own plus
//! every global secondary index it carries, in each dimension — is held to the per-table
//! throughput quota in force ("Per table – 40,000 read request units and 40,000 write request
//! units", which "restricts the sum of the provisioned capacity of the new table itself and
//! all its global secondary indexes"). A request past it is refused with the modelled
//! `LimitExceededException` and changes nothing; a request AT it is served. It is a fact of
//! the FIGURES one request states about one table, so it holds wherever a table's capacity is
//! declared or moved: the create, the update that moves it, and the declaration an import
//! creates a table from (the two restores reinstate a recorded pair through the same check).
//!
//! The quota itself comes from the crate rather than from a literal here, so each row states
//! the rule rather than one pair of numbers, and each dimension is tested AT the quota and
//! one past it.

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::{
    DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS, DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
};

use crate::emulator::{call, call_in, describe_when_active, emulator, error_code, message, ok};

/// AWS's sentence for a capacity total past the per-table quota, over the member whose total
/// is past it, the total the request asked for, the Region the account is addressed in and
/// the quota in force.
fn past_the_quota(member: &str, total: i64, region: &str, quota: i64) -> String {
    format!(
        "The requested {member}, {total}, is above the per table maximum for the account in \
         {region}. Per table maximum: {quota}. Refer to the Amazon DynamoDB Developer Guide \
         for current limits and how to request higher limits."
    )
}

/// A provisioned `CreateTable` body at the capacity pair given, with `indexes` merged in.
fn create_body(name: &str, read: i64, write: i64, indexes: Value) -> Value {
    let mut body = json!({
        "TableName": name,
        "BillingMode": "PROVISIONED",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                 {"AttributeName": "label", "AttributeType": "S"}],
        "ProvisionedThroughput": {"ReadCapacityUnits": read, "WriteCapacityUnits": write},
    });
    if indexes.is_array() {
        body["GlobalSecondaryIndexes"] = indexes;
    } else {
        // A declaration with no index declares only the table's own key attribute.
        body["AttributeDefinitions"] = json!([{"AttributeName": "pk", "AttributeType": "S"}]);
    }
    body
}

/// One `KEYS_ONLY` index on `label` at the capacity pair given.
fn index(read: i64, write: i64) -> Value {
    json!([{
        "IndexName": "by-label",
        "KeySchema": [{"AttributeName": "label", "KeyType": "HASH"}],
        "Projection": {"ProjectionType": "KEYS_ONLY"},
        "ProvisionedThroughput": {"ReadCapacityUnits": read, "WriteCapacityUnits": write},
    }]
    )
}

#[tokio::test]
async fn a_declaration_past_the_per_table_quota_is_refused_and_creates_nothing() {
    let router = emulator();
    let read_quota = DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS;
    let write_quota = DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS;

    // (case, the create body, the member the refusal names, the total it names)
    //
    // The rows differ along the dimension that is past the quota, how far past it goes, and
    // where the capacity was asked for — the table's own, an index's, and the two together,
    // because the quota bounds their SUM.
    let rows: Vec<(&str, Value, &str, i64)> = vec![
        (
            "a table one past the read quota",
            create_body("quota-read", read_quota + 1, 1, Value::Null),
            "ReadCapacityUnits",
            read_quota + 1,
        ),
        (
            "a table far past the read quota",
            create_body("quota-read-far", read_quota * 3, 1, Value::Null),
            "ReadCapacityUnits",
            read_quota * 3,
        ),
        (
            "a table one past the write quota",
            create_body("quota-write", 1, write_quota + 1, Value::Null),
            "WriteCapacityUnits",
            write_quota + 1,
        ),
        (
            "a table and an index whose read capacity sums past the quota",
            create_body("quota-sum-read", read_quota - 10, 1, index(11, 1)),
            "ReadCapacityUnits",
            read_quota + 1,
        ),
        (
            "a table and an index whose write capacity sums past the quota",
            create_body("quota-sum-write", 1, write_quota - 1, index(1, 2)),
            "WriteCapacityUnits",
            write_quota + 1,
        ),
        (
            // Both dimensions past it: the refusal names the READ member, which the model
            // declares first.
            "a table past the quota in both dimensions",
            create_body("quota-both", read_quota + 5, write_quota + 5, Value::Null),
            "ReadCapacityUnits",
            read_quota + 5,
        ),
    ];

    for (case, body, member, total) in rows {
        let name = body["TableName"].as_str().unwrap_or_default().to_string();
        let (status, refused) = call(&router, "CreateTable", body).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "LimitExceededException"),
            "case {case:?}: expected the modelled limit refusal, got {status} with {refused}"
        );
        let expected = past_the_quota(
            member,
            total,
            crate::emulator::REGION,
            if member == "ReadCapacityUnits" {
                read_quota
            } else {
                write_quota
            },
        );
        let stated = message(&refused);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the message {expected:?}, got {stated:?}"
        );

        // Nothing was created: the name the refused request asked for holds no table.
        let (described_status, described) =
            call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (described_status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: a refused create stores no table, got {described_status} with \
             {described}"
        );
    }
}

#[tokio::test]
async fn a_declaration_at_the_per_table_quota_is_served() {
    let router = emulator();
    let read_quota = DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS;
    let write_quota = DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS;

    // AT the quota in each dimension, and at it through the SUM of a table and its index:
    // the boundary is admitted, which is what makes the quota a ceiling rather than an
    // approximation. Each row reads the capacity back off the stored table.
    for (case, body, read, write) in [
        (
            "a table at the read quota",
            create_body("at-read", read_quota, 1, Value::Null),
            read_quota,
            1,
        ),
        (
            "a table at the write quota",
            create_body("at-write", 1, write_quota, Value::Null),
            1,
            write_quota,
        ),
        (
            "a table whose sum with its index is at the quota",
            create_body("at-sum", read_quota - 10, 1, index(10, 1)),
            read_quota - 10,
            1,
        ),
    ] {
        let name = body["TableName"].as_str().unwrap_or_default().to_string();
        ok(&router, "CreateTable", body).await;
        let described = describe_when_active(&router, case, &name).await;
        assert_eq!(
            (
                described["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
                described["ProvisionedThroughput"]["WriteCapacityUnits"].clone(),
            ),
            (json!(read), json!(write)),
            "case {case:?}: expected the capacity it was created with, got {described}"
        );
    }
}

#[tokio::test]
async fn a_capacity_move_past_the_quota_is_refused_and_moves_nothing() {
    let router = emulator();
    let read_quota = DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS;
    let name = "moved";
    ok(
        &router,
        "CreateTable",
        create_body(name, 10, 10, Value::Null),
    )
    .await;
    describe_when_active(&router, "the table settles before it is updated", name).await;

    let (status, refused) = call(
        &router,
        "UpdateTable",
        json!({"TableName": name,
               "ProvisionedThroughput": {"ReadCapacityUnits": read_quota + 1,
                                         "WriteCapacityUnits": 10}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "LimitExceededException"),
        "a capacity move past the quota is refused, got {status} with {refused}"
    );
    let expected = past_the_quota(
        "ReadCapacityUnits",
        read_quota + 1,
        crate::emulator::REGION,
        read_quota,
    );
    assert_eq!(
        message(&refused),
        expected,
        "expected the message {expected:?}, got {:?}",
        message(&refused)
    );

    // The refused move left the table exactly as it was — the capacity it had, and the
    // status it had settled at.
    let described = describe_when_active(&router, "after the refused move", name).await;
    assert_eq!(
        described["ProvisionedThroughput"]["ReadCapacityUnits"],
        json!(10),
        "a refused move leaves the capacity in force, got {described}"
    );

    // And the move the quota admits applies: the table reports the pair it asked for once
    // the update settles.
    ok(
        &router,
        "UpdateTable",
        json!({"TableName": name,
               "ProvisionedThroughput": {"ReadCapacityUnits": read_quota,
                                         "WriteCapacityUnits": 10}}),
    )
    .await;
    let moved = crate::emulator::table_when(
        &router,
        "the admitted move settles",
        crate::emulator::REGION,
        name,
        |table| table["ProvisionedThroughput"]["ReadCapacityUnits"] == json!(read_quota),
    )
    .await;
    assert_eq!(
        moved["ProvisionedThroughput"]["ReadCapacityUnits"],
        json!(read_quota),
        "the admitted move applies the pair it asked for, got {moved}"
    );
}

#[tokio::test]
async fn an_import_declaration_past_the_quota_is_refused_too() {
    let router = emulator();
    let read_quota = DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS;

    // The same rule over the OTHER member a table is declared in,
    // `ImportTable$TableCreationParameters`, and in another REGION — which the sentence
    // names, because a quota is held per account and Region.
    let region = "eu-west-1";
    let (status, refused) = call_in(
        &router,
        region,
        "ImportTable",
        json!({
            "S3BucketSource": {"S3Bucket": "declarations"},
            "InputFormat": "DYNAMODB_JSON",
            "TableCreationParameters": {
                "TableName": "imported-past-the-quota",
                "BillingMode": "PROVISIONED",
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "ProvisionedThroughput": {"ReadCapacityUnits": read_quota + 1,
                                          "WriteCapacityUnits": 1},
            },
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "LimitExceededException"),
        "an import declaration past the quota is refused, got {status} with {refused}"
    );
    let expected = past_the_quota("ReadCapacityUnits", read_quota + 1, region, read_quota);
    assert_eq!(
        message(&refused),
        expected,
        "expected the message {expected:?}, got {:?}",
        message(&refused)
    );

    // Nothing was created, and no import was started: the name holds no table.
    let (described_status, described) = call_in(
        &router,
        region,
        "DescribeTable",
        json!({"TableName": "imported-past-the-quota"}),
    )
    .await;
    assert_eq!(
        (described_status, error_code(&described).as_str()),
        (400, "ResourceNotFoundException"),
        "a refused import stores no table, got {described_status} with {described}"
    );
    let listed = call_in(&router, region, "ListImports", json!({})).await;
    assert_eq!(
        listed.1["ImportSummaryList"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        0,
        "a refused import starts nothing, got {}",
        listed.1
    );
}
