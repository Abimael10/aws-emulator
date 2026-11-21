// SPDX-License-Identifier: CC0-1.0

//! WHICH SENTENCE the replica auto scaling pair refuses an unaddressable name with.
//!
//! The rule these tests pin:
//!
//! > `DescribeTableReplicaAutoScaling` and `UpdateTableReplicaAutoScaling` are stated over
//! > the global table, so a `TableName` no replication group of the Region the request
//! > reached answers to is refused with HTTP 400 `ResourceNotFoundException` carrying the
//! > sentence DynamoDB states for a missing global table — fixed text `Global table with
//! > name: '` and `' does not exist.` around the table name the request addressed — and
//! > never the generic table-resolution sentence the operations that address the TABLE
//! > state. The refusal writes nothing.
//!
//! The rows span what the rule generalises over: the FORM of the admissible name (the
//! shortest and longest the member admits, every character class it admits, and the ARN
//! spelling of a name, whose refusal states the table name the ARN resolves to), the
//! operation sent (both of the pair) and the members an update carries, the endpoint Region
//! the request is signed for, and WHY the name answers no group (no table at all, or a
//! table that replicates nowhere).
//!
//! The bounds and character set the name rows are built from are the
//! `CreateTable$TableName` constraint — 3..=255 characters of `[a-zA-Z0-9_.-]`
//! (`smithy_query(query="doc",shape="com.amazonaws.dynamodb#CreateTableInput",member="TableName")`);
//! a name outside them is refused for its FORM instead and so is another rule's row.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call_in, emulator, error_code, message, ok_in, REGION};

/// The account a table's ARN is minted under, for the row that addresses a name by its ARN.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// A second endpoint Region, so the rule is driven over more than one: a table name is
/// unique within ONE Region, so a name is answered — or not — per endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The shortest and the longest name the `TableName` member admits (3..=255 characters).
const MIN_TABLE_NAME_LENGTH: usize = 3;
const MAX_TABLE_NAME_LENGTH: usize = 255;

/// The sentence a request addressing `name` must be refused with.
fn missing_global_table(name: &str) -> String {
    format!("Global table with name: '{name}' does not exist.")
}

/// One `AutoScalingSettingsUpdate` body: the capacity window and the policy steering it.
fn settings_update(minimum: i64, maximum: i64) -> Value {
    json!({
        "MinimumUnits": minimum,
        "MaximumUnits": maximum,
        "ScalingPolicyUpdate": {
            "PolicyName": "test-policy",
            "TargetTrackingScalingPolicyConfiguration": {"TargetValue": 70.0},
        },
    })
}

/// Every request of the pair, as `(form, operation, body)`: the describe, and the update in
/// each of the forms its members take — the group's write capacity, one replica's read
/// capacity, and a named index's.
fn requests(table_name: &str) -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "describing the settings",
            "DescribeTableReplicaAutoScaling",
            json!({"TableName": table_name}),
        ),
        (
            "updating the group's write capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": table_name,
                "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(1, 10),
            }),
        ),
        (
            "updating one replica's read capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": table_name,
                "ReplicaUpdates": [{
                    "RegionName": REGION,
                    "ReplicaProvisionedReadCapacityAutoScalingUpdate": settings_update(2, 20),
                }],
            }),
        ),
        (
            "updating an index's write capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": table_name,
                "GlobalSecondaryIndexUpdates": [{
                    "IndexName": "ByStatus",
                    "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(3, 30),
                }],
            }),
        ),
    ]
}

/// Send every request of the pair for `sent` at `region` and assert each is refused with the
/// global-table sentence naming `stated`.
async fn assert_global_table_refusal(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    sent: &str,
    stated: &str,
) {
    for (form, operation, body) in requests(sent) {
        let (status, answered) = call_in(router, region, operation, body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}, {form} in {region}: expected HTTP 400 ResourceNotFoundException, \
             got {status} with {answered}"
        );
        let said = message(&answered);
        assert_eq!(
            said,
            missing_global_table(stated),
            "case {case:?}, {form} in {region}: expected the missing-global-table sentence, \
             got {said:?}"
        );
    }
}

/// One provisioned table carrying the index an update row names, created in `region`.
async fn plain_table(router: &MultiServiceRouter, region: &str, name: &str) {
    ok_in(
        router,
        region,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "Id", "AttributeType": "S"},
                {"AttributeName": "Status", "AttributeType": "S"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByStatus",
                "KeySchema": [{"AttributeName": "Status", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            }],
        }),
    )
    .await;
}

#[tokio::test]
async fn a_name_no_table_answers_is_refused_with_the_global_table_sentence_in_every_form() {
    let router = emulator();
    let shortest = "a".repeat(MIN_TABLE_NAME_LENGTH);
    let longest = "L".repeat(MAX_TABLE_NAME_LENGTH);
    let every_class = "Wording.absent_name-09";
    // `(case, endpoint Region, the value sent as TableName, the name the sentence states)`.
    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "the shortest name the member admits",
            REGION,
            shortest.clone(),
            shortest.clone(),
        ),
        (
            "the longest name the member admits",
            REGION,
            longest.clone(),
            longest.clone(),
        ),
        (
            "a name spelt with every admitted character class",
            REGION,
            every_class.to_string(),
            every_class.to_string(),
        ),
        (
            "the ARN spelling of a name no table answers",
            REGION,
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{every_class}"),
            every_class.to_string(),
        ),
        (
            "the same name at another endpoint",
            OTHER_REGION,
            every_class.to_string(),
            every_class.to_string(),
        ),
    ];

    for (case, region, sent, stated) in &rows {
        assert_global_table_refusal(&router, case, region, sent, stated).await;
    }

    // Nothing the refusals touched came into existence: the name still answers no table at
    // either endpoint, so the refused updates wrote no group and no table.
    for region in [REGION, OTHER_REGION] {
        let (status, answered) = call_in(
            &router,
            region,
            "DescribeTable",
            json!({"TableName": every_class}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ResourceNotFoundException"),
            "case a refused pair writes nothing in {region}: the name must still answer no \
             table, got {status} with {answered}"
        );
    }
}

#[tokio::test]
async fn a_table_that_replicates_nowhere_is_refused_as_a_missing_global_table() {
    let router = emulator();
    let name = "Wording.regional";
    plain_table(&router, REGION, name).await;

    // The Region that HOLDS the table: it answers a table, but not a replication group, so
    // the pair reports a missing global table rather than the table it found.
    assert_global_table_refusal(
        &router,
        "a table of this Region that replicates nowhere",
        REGION,
        name,
        name,
    )
    .await;
    // And the Region that holds no table of that name at all is refused identically — the
    // two misses are one rule, so one sentence.
    assert_global_table_refusal(
        &router,
        "the same name at an endpoint holding no such table",
        OTHER_REGION,
        name,
        name,
    )
    .await;

    // The refused updates left the table exactly as the create made it: no replica, and the
    // capacities the create stated.
    let described = ok_in(&router, REGION, "DescribeTable", json!({"TableName": name})).await
        ["Table"]
        .clone();
    assert_eq!(
        (
            described["Replicas"].clone(),
            described["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
            described["ProvisionedThroughput"]["WriteCapacityUnits"].clone(),
        ),
        (Value::Null, json!(5), json!(5)),
        "case a refused pair writes nothing: the table must be as created, got {described}"
    );
}
