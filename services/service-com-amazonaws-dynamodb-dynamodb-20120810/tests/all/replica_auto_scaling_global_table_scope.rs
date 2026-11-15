// SPDX-License-Identifier: CC0-1.0

//! WHAT the replica auto scaling pair addresses: the GLOBAL TABLE, not the table.
//!
//! The rule these tests pin:
//!
//! > `DescribeTableReplicaAutoScaling` and `UpdateTableReplicaAutoScaling` are stated over
//! > the global table — "describes auto scaling settings across replicas of the global
//! > table at once" and "updates auto scaling settings on your global tables at once" — so
//! > a `TableName` that is not a global table's addresses no resource they can answer for:
//! > both refuse it with HTTP 400 `ResourceNotFoundException` and the sentence DynamoDB
//! > states for a missing global table, `Global table with name: '<TableName>' does not
//! > exist.`, and the update writes nothing. A name is a global table's only while the
//! > table it names belongs to a replication group, so the SAME name is refused before a
//! > replica is created and served once one is.
//!
//! The rows span what the rule generalises over: the name addressed (one no table answers
//! to, one answering a plain table, one answering a table that had a replica and lost it),
//! the operation sent (both of the pair), the members the update carries (the group's write
//! capacity, one replica's read capacity, an index's), and the Region the request is signed
//! for.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call_in, emulator, error_code, message, ok_in, REGION};

/// The Region a replica is created in, so a table of [`REGION`] becomes a global table.
const REPLICA_REGION: &str = "eu-west-1";

/// The global secondary index each table below carries, so an update may address an index
/// too.
const INDEX: &str = "ByStatus";

/// Wait out one transient span, so whatever the clock owes a table or a replica is due.
async fn settle() {
    tokio::time::sleep(
        service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION
            + std::time::Duration::from_millis(150),
    )
    .await;
}

/// One provisioned table with a global secondary index, created in [`REGION`] and settled.
async fn plain_table(router: &MultiServiceRouter, name: &str) {
    ok_in(
        router,
        REGION,
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
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "Status", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            }],
        }),
    )
    .await;
    settle().await;
}

/// Add (or remove) the replica in [`REPLICA_REGION`], which is what makes `name` a global
/// table (or stops it being one), and wait for the change to settle.
async fn set_replica(router: &MultiServiceRouter, name: &str, present: bool) {
    let action = if present { "Create" } else { "Delete" };
    ok_in(
        router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": name,
            "ReplicaUpdates": [{action: {"RegionName": REPLICA_REGION}}],
        }),
    )
    .await;
    settle().await;
}

/// One `AutoScalingSettingsUpdate` body: the capacity window and the policy that steers it.
fn settings_update(minimum: i64, maximum: i64) -> Value {
    json!({
        "MinimumUnits": minimum,
        "MaximumUnits": maximum,
        "ScalingPolicyUpdate": {
            "TargetTrackingScalingPolicyConfiguration": {"TargetValue": 70.0},
        },
    })
}

/// Every request of the pair this rule is driven over, as `(case, operation, body builder)`:
/// the describe, and the update in each of the forms its members take — the group's write
/// capacity, one replica's read capacity, and an index's on each side.
fn requests(name: &str) -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "describing the auto scaling settings",
            "DescribeTableReplicaAutoScaling",
            json!({"TableName": name}),
        ),
        (
            "updating the group's write capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": name,
                "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(2, 20),
            }),
        ),
        (
            "updating one replica's read capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": name,
                "ReplicaUpdates": [{
                    "RegionName": REGION,
                    "ReplicaProvisionedReadCapacityAutoScalingUpdate": settings_update(1, 11),
                }],
            }),
        ),
        (
            "updating an index's write capacity",
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": name,
                "GlobalSecondaryIndexUpdates": [{
                    "IndexName": INDEX,
                    "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(3, 30),
                }],
            }),
        ),
    ]
}

/// Assert every request of the pair is refused for `name` with the global-table refusal.
async fn assert_refused(router: &MultiServiceRouter, state: &str, name: &str) {
    for (case, operation, body) in requests(name) {
        let (status, answered) = call_in(router, REGION, operation, body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ResourceNotFoundException"),
            "case {state}, {case}: expected HTTP 400 ResourceNotFoundException, got {status} \
             with {answered}"
        );
        assert_eq!(
            message(&answered),
            format!("Global table with name: '{name}' does not exist."),
            "case {state}, {case}: the refusal states the global table the request named"
        );
    }
}

#[tokio::test]
async fn a_name_that_is_not_a_global_tables_is_refused_by_both_operations() {
    let router = emulator();
    // Each row is a name that is not a global table's, and so is refused the same way: a
    // name no table answers to at all, and one answering a table of a single Region.
    plain_table(&router, "Scoped.plain").await;
    let rows: [(&str, &str); 2] = [
        ("a name no table answers to", "Scoped.absent"),
        ("a table that replicates nowhere", "Scoped.plain"),
    ];
    for (state, name) in rows {
        assert_refused(&router, state, name).await;
    }
}

#[tokio::test]
async fn the_same_name_is_served_once_it_names_a_global_table() {
    let router = emulator();
    let name = "Scoped.replicated";
    plain_table(&router, name).await;
    // Before: the name answers a table of one Region, so it is not a global table's.
    assert_refused(&router, "before any replica is created", name).await;

    // After: the same name answers a replication group, so every request of the pair is
    // served — and what the update wrote reads back through the describing operation.
    set_replica(&router, name, true).await;
    for (case, operation, body) in requests(name) {
        let (status, answered) = call_in(&router, REGION, operation, body).await;
        assert_eq!(
            status, 200,
            "case a global table, {case}: expected HTTP 200, got {status} with {answered}"
        );
    }
    let described = ok_in(
        &router,
        REGION,
        "DescribeTableReplicaAutoScaling",
        json!({"TableName": name}),
    )
    .await["TableAutoScalingDescription"]
        .clone();
    let replica = described["Replicas"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|replica| replica["RegionName"] == json!(REGION))
        .unwrap_or(Value::Null);
    assert_eq!(
        (
            described["TableName"].clone(),
            described["Replicas"].as_array().map(Vec::len),
            replica["ReplicaProvisionedWriteCapacityAutoScalingSettings"]["MaximumUnits"].clone(),
            replica["ReplicaProvisionedReadCapacityAutoScalingSettings"]["MaximumUnits"].clone(),
        ),
        (json!(name), Some(2), json!(20), json!(11)),
        "case a global table: the description reports the group and what the updates wrote, \
         got {described}"
    );

    // And after the replica is removed the name is not a global table's again, so the pair
    // refuses it exactly as it did before the replica existed.
    set_replica(&router, name, false).await;
    assert_refused(&router, "after the replica is removed", name).await;
}
