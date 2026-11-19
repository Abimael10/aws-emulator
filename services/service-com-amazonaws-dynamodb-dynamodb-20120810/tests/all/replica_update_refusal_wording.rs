// SPDX-License-Identifier: CC0-1.0

//! How `UpdateTable`'s `ReplicaUpdates` WORDS its refusals, and what a replica reports
//! back of the throughput it overrides.
//!
//! The rules these tests pin:
//!
//! > 1. an entry that ADDS or REMOVES the Region the table itself is in is refused with the
//! >    `ValidationException` AWS words for it — fixed text naming the operations that add
//! >    and remove a regional table instead, with nothing taken from the request;
//! > 2. an `Update` or `Delete` entry naming a Region the table has no replica in is
//! >    refused with AWS's sentence for a replica the action did not find, which is
//! >    distinct from the one above so the two reasons stay apart;
//! > 3. a `Create` entry naming a Region that already holds a table of the group's name —
//! >    whether that table is a replica of this group or a table of its own — is refused
//! >    with AWS's sentence for it, whose one variable is the table name the request named;
//! > 4. a replica keeps warm the read capacity it overrides: the override is reported back
//! >    under both `Replicas[].ProvisionedThroughputOverride.ReadCapacityUnits` and
//! >    `Replicas[].WarmThroughput.ReadUnitsPerSecond`, and an `Update` that changes the
//! >    override moves both.
//!
//! The rows of each table differ along what the rules generalise over: the action the entry
//! carries, the Region it names, the capacity the override states, and whether the Region it
//! names holds a replica, an unrelated table of the same name, or no table at all.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call_in, emulator, error_code, message, ok_in, table_when, REGION};

/// AWS's sentence for an entry that adds or removes the table's own Region.
const LOCAL_REGION_MESSAGE: &str = "Cannot add or delete the local region through \
                                    ReplicaUpdates. Use CreateTable, DeleteTable, or \
                                    UpdateTable as required.";

/// AWS's sentence for an `Update` or `Delete` entry whose Region holds no replica.
const REPLICA_NOT_FOUND_MESSAGE: &str = "Replica specified in the Replica Update or Replica \
                                         Delete action of the request was not found.";

/// AWS's sentence for a `Create` entry whose Region already holds a table of the group's
/// name, whose one variable is that name.
fn already_exists_message(name: &str) -> String {
    format!(
        "Failed to create a the new replica of table with name: \u{2018}{name}\u{2019} because \
         one or more replicas already existed as tables."
    )
}

/// One table in `region`: PAY_PER_REQUEST unless `read_capacity` states a provisioned read
/// capacity, which is what makes a replica's read override observable.
async fn create_table(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    read_capacity: Option<i64>,
) {
    let mut body = json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
    });
    match read_capacity {
        Some(units) => {
            body["BillingMode"] = json!("PROVISIONED");
            body["ProvisionedThroughput"] = json!({
                "ReadCapacityUnits": units,
                "WriteCapacityUnits": 4,
            });
        }
        None => body["BillingMode"] = json!("PAY_PER_REQUEST"),
    }
    ok_in(router, region, "CreateTable", body).await;
}

/// The `Replicas` entry a description carries for `region`, or `Null` when it carries none.
fn replica_in(description: &Value, region: &str) -> Value {
    description["Replicas"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|replica| replica["RegionName"] == json!(region))
        .unwrap_or(Value::Null)
}

/// Whether every replica a description reports is `ACTIVE`, which is where a group settles.
fn replicas_active(table: &Value) -> bool {
    table["Replicas"]
        .as_array()
        .is_some_and(|replicas| {
            !replicas.is_empty()
                && replicas
                    .iter()
                    .all(|replica| replica["ReplicaStatus"] == json!("ACTIVE"))
        })
}

/// One row of the refusal table: the case, the `ReplicaUpdates` list, and the message AWS
/// refuses it with.
type RefusalRow = (&'static str, Value, String);

#[tokio::test]
async fn an_entry_that_cannot_apply_is_refused_with_aws_s_own_sentence_for_its_reason() {
    let router = emulator();
    let name = "Replicated.Wording";
    create_table(&router, REGION, name, None).await;
    table_when(&router, "the table", REGION, name, |table| {
        table["TableStatus"] == json!("ACTIVE")
    })
    .await;
    ok_in(
        &router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": name,
            "ReplicaUpdates": [{"Create": {"RegionName": "us-west-2"}}],
        }),
    )
    .await;
    table_when(&router, "the replica", REGION, name, replicas_active).await;
    // A table of the group's name that is NOT a replica of it, in a third Region: a
    // `Create` naming that Region is refused for the same reason a Region already holding a
    // replica is.
    create_table(&router, "eu-west-1", name, None).await;

    let rows: [RefusalRow; 8] = [
        (
            "a Create naming the table's own Region",
            json!([{"Create": {"RegionName": REGION}}]),
            LOCAL_REGION_MESSAGE.to_string(),
        ),
        (
            "a Delete naming the table's own Region",
            json!([{"Delete": {"RegionName": REGION}}]),
            LOCAL_REGION_MESSAGE.to_string(),
        ),
        (
            "a Create naming the table's own Region with overrides",
            json!([{"Create": {
                "RegionName": REGION,
                "ProvisionedThroughputOverride": {"ReadCapacityUnits": 3},
            }}]),
            LOCAL_REGION_MESSAGE.to_string(),
        ),
        (
            "a Delete naming a Region that holds no table at all",
            json!([{"Delete": {"RegionName": "ap-south-1"}}]),
            REPLICA_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a Delete naming a Region holding a table that is no replica",
            json!([{"Delete": {"RegionName": "eu-west-1"}}]),
            REPLICA_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "an Update naming a Region that holds no replica",
            json!([{"Update": {
                "RegionName": "us-gov-east-1",
                "ProvisionedThroughputOverride": {"ReadCapacityUnits": 2},
            }}]),
            REPLICA_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a Create naming a Region the group already holds",
            json!([{"Create": {"RegionName": "us-west-2"}}]),
            already_exists_message(name),
        ),
        (
            "a Create naming a Region holding a table that is no replica",
            json!([{"Create": {"RegionName": "eu-west-1"}}]),
            already_exists_message(name),
        ),
    ];

    for (case, updates, expected) in rows {
        let (status, body) = call_in(
            &router,
            REGION,
            "UpdateTable",
            json!({"TableName": name, "ReplicaUpdates": updates.clone()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected AWS's sentence {expected:?} for {updates}, got {status} \
             with {body}"
        );
    }

    // Nothing moved: the group still spans the two Regions it did, and the unrelated table
    // in the third Region is still a table of its own.
    let described =
        table_when(&router, "after the refusals", REGION, name, replicas_active).await;
    let standalone = ok_in(
        &router,
        "eu-west-1",
        "DescribeTable",
        json!({"TableName": name}),
    )
    .await;
    assert_eq!(
        (
            described["Replicas"].as_array().map(Vec::len),
            replica_in(&described, "us-west-2")["ReplicaStatus"].clone(),
            standalone["Table"]["Replicas"].clone(),
        ),
        (Some(1), json!("ACTIVE"), Value::Null),
        "case after the refusals: the group is untouched, got {described} and {standalone}"
    );
}

#[tokio::test]
async fn a_replica_reports_the_read_capacity_it_keeps_warm_as_the_override_it_was_given() {
    // RULE: warm throughput is the minimum provisioned capacity a resource keeps for
    // immediate data access, so a replica that overrides its read capacity keeps THAT many
    // read units warm — which is what a client reads the override back as, through both
    // members that carry it.
    let router = emulator();
    let name = "Replicated.Overridden";
    create_table(&router, REGION, name, Some(5)).await;
    table_when(&router, "the table", REGION, name, |table| {
        table["TableStatus"] == json!("ACTIVE")
    })
    .await;
    ok_in(
        &router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": name,
            "ReplicaUpdates": [{"Create": {
                "RegionName": "us-west-2",
                "ProvisionedThroughputOverride": {"ReadCapacityUnits": 7},
            }}],
        }),
    )
    .await;

    // Rows differ in the capacity the override states: the one the create named, then one
    // above it, one below it and one that restates the source table's own read capacity.
    let rows: [(&str, i64); 4] = [
        ("the capacity the create named", 7),
        ("a capacity above it", 9),
        ("a capacity below it", 2),
        ("the source table's own read capacity", 5),
    ];
    for (index, (case, units)) in rows.into_iter().enumerate() {
        if index > 0 {
            ok_in(
                &router,
                REGION,
                "UpdateTable",
                json!({
                    "TableName": name,
                    "ReplicaUpdates": [{"Update": {
                        "RegionName": "us-west-2",
                        "ProvisionedThroughputOverride": {"ReadCapacityUnits": units},
                    }}],
                }),
            )
            .await;
        }
        // The poll a client's wait-for-settled loop runs: the replica is ACTIVE and reports
        // the capacity it was asked for.
        let described = table_when(&router, case, REGION, name, |table| {
            replicas_active(table)
                && replica_in(table, "us-west-2")["WarmThroughput"]["ReadUnitsPerSecond"]
                    == json!(units)
        })
        .await;
        let replica = replica_in(&described, "us-west-2");
        assert_eq!(
            (
                replica["ProvisionedThroughputOverride"]["ReadCapacityUnits"].clone(),
                replica["WarmThroughput"]["ReadUnitsPerSecond"].clone(),
                replica["WarmThroughput"]["WriteUnitsPerSecond"].clone(),
                described["WarmThroughput"]["ReadUnitsPerSecond"].clone(),
            ),
            (json!(units), json!(units), json!(4), json!(5)),
            "case {case:?}: expected the replica to report {units} read units warm and the \
             source table its own, got {described}"
        );
        // ...and the replica's own regional table is provisioned for that capacity.
        let own = ok_in(
            &router,
            "us-west-2",
            "DescribeTable",
            json!({"TableName": name}),
        )
        .await;
        assert_eq!(
            own["Table"]["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
            json!(units),
            "case {case:?}: expected the replica's own table provisioned for {units} read \
             units, got {own}"
        );
    }

    // A replica that overrides NOTHING keeps the source table's pair warm, which is the
    // other half of the rule.
    ok_in(
        &router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": name,
            "ReplicaUpdates": [{"Create": {"RegionName": "eu-west-1"}}],
        }),
    )
    .await;
    let described = table_when(&router, "a replica with no override", REGION, name, |table| {
        replicas_active(table) && replica_in(table, "eu-west-1") != Value::Null
    })
    .await;
    let plain = replica_in(&described, "eu-west-1");
    assert_eq!(
        (
            plain["ProvisionedThroughputOverride"].clone(),
            plain["WarmThroughput"]["ReadUnitsPerSecond"].clone(),
        ),
        (Value::Null, json!(5)),
        "case a replica with no override: expected no override and the source table's warm \
         read units, got {described}"
    );
}
