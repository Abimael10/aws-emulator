// SPDX-License-Identifier: CC0-1.0

//! How the LEGACY (2017.11.29) global-table operations WORD their refusals.
//!
//! The rules these tests pin:
//!
//! > 1. the version those operations address is not offered, so `CreateGlobalTable` is
//! >    refused with the `ValidationException` AWS words for that version — fixed text
//! >    naming the version that is not supported and the one to use instead, with nothing
//! >    taken from the request;
//! > 2. every legacy operation that ADDRESSES one global table answers the modelled
//! >    `GlobalTableNotFoundException` whose message states AWS's sentence for that error,
//! >    "Global table with name: '<name>' does not exist.", whose one variable is the name
//! >    the request asked about — behind `DescribeGlobalTable`'s own "Global table not
//! >    found: " clause on that operation, and on its own on the SETTINGS operations;
//! > 3. `UpdateGlobalTable`, which would CHANGE such a global table rather than only read
//! >    one, is refused by whichever of the two reasons the store decides: the not-found
//! >    error for a name no table of the Region carries, and the version refusal for a name
//! >    one does.
//!
//! The rows of each table differ along what the rules generalise over: the operation
//! called, the name the request addresses (a replicated table, a standalone table, a name
//! no table carries, and names of different shapes and lengths), and the Region the call
//! is signed for — none of which changes the refusal, because the version itself is what
//! is refused.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call_in, describe_when_active, emulator, error_code, message, ok_in, REGION,
};

/// AWS's sentence for a call on the withdrawn version, which no part of a request can
/// change.
const LEGACY_VERSION_MESSAGE: &str = "One or more parameter values were invalid: DynamoDB \
                                      global tables version 2017.11.29 is not supported. We \
                                      recommend using DynamoDB global tables version \
                                      2019.11.21, instead of version 2017.11.29 (Legacy).";

/// The not-found sentence for the global table `name`, whose only variable is that name.
fn does_not_exist(name: &str) -> String {
    format!("Global table with name: '{name}' does not exist.")
}

/// One table in `region`, created over the wire so the names the refusals address include
/// tables that really exist.
async fn create_table(router: &MultiServiceRouter, region: &str, name: &str) {
    ok_in(
        router,
        region,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
}

/// The names the refusals below address: a table that exists, one of the shortest name the
/// model admits, one of a long name carrying every admitted character class, and one no
/// table carries.
const ADDRESSED_NAMES: [(&str, &str); 4] = [
    ("a name a table carries", "Legacy.Existing"),
    ("the shortest name the model admits", "a_b"),
    (
        "a long name of every admitted character class",
        "Legacy_global-table.2017.11.29-Name",
    ),
    ("a name no table carries", "Legacy.Absent"),
];

#[tokio::test]
async fn create_global_table_is_refused_with_the_withdrawn_version_sentence() {
    let router = emulator();
    create_table(&router, REGION, "Legacy.Existing").await;

    // Rows differ in the name the create addresses and in the Regions its replication
    // group names: the version is what is refused, so none of them changes the answer.
    let groups: [(&str, Value); 3] = [
        ("one Region", json!([{"RegionName": "us-west-2"}])),
        (
            "two Regions",
            json!([{"RegionName": "us-west-2"}, {"RegionName": "eu-west-1"}]),
        ),
        ("the Region of the call itself", json!([{"RegionName": REGION}])),
    ];

    for (name_case, name) in ADDRESSED_NAMES {
        for (group_case, replication_group) in groups.clone() {
            let (status, body) = call_in(
                &router,
                REGION,
                "CreateGlobalTable",
                json!({"GlobalTableName": name, "ReplicationGroup": replication_group}),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str(), message(&body)),
                (400, "ValidationException", LEGACY_VERSION_MESSAGE.to_string()),
                "case {name_case:?} with {group_case}: expected AWS's withdrawn-version \
                 sentence, got {status} with {body}"
            );
        }
    }
}

#[tokio::test]
async fn describe_global_table_states_the_not_found_sentence_behind_its_own_clause() {
    let router = emulator();
    create_table(&router, REGION, "Legacy.Existing").await;
    // A table made global the CURRENT way is one the legacy surface cannot see either, so
    // it is addressed beside the names above.
    create_table(&router, REGION, "Legacy.Replicated").await;
    // "You can perform read and write operations only on an ACTIVE table", so the replica
    // is added once that service reports the table out of its creation window.
    describe_when_active(&router, "the table made global", "Legacy.Replicated").await;
    ok_in(
        &router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": "Legacy.Replicated",
            "ReplicaUpdates": [{"Create": {"RegionName": "us-west-2"}}],
        }),
    )
    .await;

    for (case, name) in ADDRESSED_NAMES
        .into_iter()
        .chain([("a table made global the current way", "Legacy.Replicated")])
    {
        let (status, body) = call_in(
            &router,
            REGION,
            "DescribeGlobalTable",
            json!({"GlobalTableName": name}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (
                400,
                "GlobalTableNotFoundException",
                format!("Global table not found: {}", does_not_exist(name)),
            ),
            "case {case:?}: expected the prefixed not-found sentence for {name}, got {status} \
             with {body}"
        );
    }
}

#[tokio::test]
async fn the_settings_operations_state_the_not_found_sentence_on_its_own() {
    let router = emulator();
    create_table(&router, REGION, "Legacy.Existing").await;

    // Rows differ in the operation called and in what it names beside the global table:
    // both operations of the settings surface answer the same sentence, with no clause of
    // their own around it.
    for (case, name) in ADDRESSED_NAMES {
        let calls: [(&str, Value); 3] = [
            ("DescribeGlobalTableSettings", json!({"GlobalTableName": name})),
            (
                "UpdateGlobalTableSettings",
                json!({"GlobalTableName": name, "GlobalTableBillingMode": "PAY_PER_REQUEST"}),
            ),
            (
                "UpdateGlobalTableSettings",
                json!({
                    "GlobalTableName": name,
                    "GlobalTableProvisionedWriteCapacityUnits": 5,
                }),
            ),
        ];
        for (operation, request) in calls {
            let (status, body) = call_in(&router, REGION, operation, request.clone()).await;
            assert_eq!(
                (status, error_code(&body).as_str(), message(&body)),
                (
                    400,
                    "GlobalTableNotFoundException",
                    does_not_exist(name),
                ),
                "case {case:?} on {operation} with {request}: expected the bare not-found \
                 sentence, got {status} with {body}"
            );
        }
    }
}

#[tokio::test]
async fn update_global_table_is_refused_by_the_reason_the_store_decides() {
    // RULE: `UpdateGlobalTable` would CHANGE the replication group of a global table of the
    // withdrawn version, so what it is refused with depends on the store: a name no table of
    // the Region carries addresses nothing, which is the modelled not-found error; a name a
    // table DOES carry addresses a group of a version that is not on offer, which is the
    // same `ValidationException` the create is refused with.
    let router = emulator();
    create_table(&router, REGION, "Legacy.Existing").await;
    describe_when_active(&router, "the table the update names", "Legacy.Existing").await;
    create_table(&router, REGION, "Legacy.Replicated").await;
    describe_when_active(&router, "the table made global", "Legacy.Replicated").await;
    ok_in(
        &router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": "Legacy.Replicated",
            "ReplicaUpdates": [{"Create": {"RegionName": "us-west-2"}}],
        }),
    )
    .await;

    // Rows differ in whether the name addresses a table of this Region (a standalone one, a
    // replicated one) and in the replica updates the request carries: the reason is decided
    // by the table, not by the updates.
    let rows: [(&str, &str, Value, &str, String); 4] = [
        (
            "a standalone table of this Region",
            "Legacy.Existing",
            json!([{"Create": {"RegionName": "eu-west-1"}}]),
            "ValidationException",
            LEGACY_VERSION_MESSAGE.to_string(),
        ),
        (
            "a table made global the current way",
            "Legacy.Replicated",
            json!([{"Delete": {"RegionName": "us-west-2"}}]),
            "ValidationException",
            LEGACY_VERSION_MESSAGE.to_string(),
        ),
        (
            "a name no table of this Region carries",
            "Legacy.Absent",
            json!([{"Create": {"RegionName": "eu-west-1"}}]),
            "GlobalTableNotFoundException",
            format!("Global table not found: {}", does_not_exist("Legacy.Absent")),
        ),
        (
            "the shortest name the model admits, which no table carries",
            "a_b",
            json!([{"Delete": {"RegionName": "eu-west-1"}}]),
            "GlobalTableNotFoundException",
            format!("Global table not found: {}", does_not_exist("a_b")),
        ),
    ];

    for (case, name, replica_updates, code, expected) in rows {
        let (status, body) = call_in(
            &router,
            REGION,
            "UpdateGlobalTable",
            json!({"GlobalTableName": name, "ReplicaUpdates": replica_updates}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, code, expected.clone()),
            "case {case:?}: expected {code} saying {expected:?}, got {status} with {body}"
        );
    }

    // A table the refused call named is untouched: the group it belongs to is the one
    // `ReplicaUpdates` made, and no Region the legacy call named gained a table.
    let described = ok_in(
        &router,
        REGION,
        "DescribeTable",
        json!({"TableName": "Legacy.Replicated"}),
    )
    .await;
    let (status, absent) = call_in(
        &router,
        "eu-west-1",
        "DescribeTable",
        json!({"TableName": "Legacy.Replicated"}),
    )
    .await;
    assert_eq!(
        (
            described["Table"]["Replicas"]
                .as_array()
                .map(Vec::len),
            (status, error_code(&absent)),
        ),
        (Some(1), (400, "ResourceNotFoundException".to_string())),
        "case after the legacy refusals: nothing was created, got {described} and {absent}"
    );
}

#[tokio::test]
async fn the_refusals_are_the_same_whichever_region_the_call_is_signed_for() {
    let router = emulator();
    // The version is a property of the SERVICE rather than of an endpoint, so the same
    // call made in other Regions is refused the same way.
    for region in ["us-east-1", "eu-west-1", "ap-south-1"] {
        let (status, body) = call_in(
            &router,
            region,
            "CreateGlobalTable",
            json!({
                "GlobalTableName": "Legacy.Regional",
                "ReplicationGroup": [{"RegionName": "us-west-2"}],
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", LEGACY_VERSION_MESSAGE.to_string()),
            "case the create in {region}: expected AWS's withdrawn-version sentence, got \
             {status} with {body}"
        );
        let (status, body) = call_in(
            &router,
            region,
            "DescribeGlobalTableSettings",
            json!({"GlobalTableName": "Legacy.Regional"}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (
                400,
                "GlobalTableNotFoundException",
                does_not_exist("Legacy.Regional"),
            ),
            "case the settings describe in {region}: expected the bare not-found sentence, \
             got {status} with {body}"
        );
    }
}
