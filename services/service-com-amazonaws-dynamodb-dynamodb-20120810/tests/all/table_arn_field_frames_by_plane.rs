// SPDX-License-Identifier: CC0-1.0

//! WHICH FRAME a refusal about a FIELD of the ARN a `TableName` carried is stated in, per
//! PLANE, driven end to end over this service's own wire protocol.
//!
//! RULE under test: a `TableName` may be spelled as the table's ARN ("You can also provide
//! the Amazon Resource Name (ARN) of the table in this parameter" —
//! `smithy_query(query="doc",shape="com.amazonaws.dynamodb#PutItemInput",member="TableName")`),
//! and an ARN whose own FIELD is wrong for a table of this endpoint — its resource type, or
//! its region — is refused for that field. An operation of the DATA plane, which reads or
//! writes the ITEMS of the table its request-level `TableName` addresses, states that
//! diagnosis as the `tableName` member's own constraint clause, with the ARN the request
//! sent echoed; a CONTROL-plane operation, which acts on the table or mints a resource of
//! its own, states it as the diagnosis sentence over the ARN.
//!
//! The rows differ along what the rule generalises over: the OPERATION (every data-plane
//! operation whose request carries a `TableName`, reads and writes alike, against
//! control-plane rows for the contrast), the FIELD of the ARN that is wrong, and the way
//! it is wrong. The last test drives the table's OWN ARN through the same data-plane
//! operations, so the refusals are about the field and not about an ARN in the member.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok, REGION};
use crate::table_items::create_active_table;

/// The table every row addresses.
const TABLE: &str = "PlaneFrames";

/// The account id every generated emulator presents (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// A Region this endpoint does not serve, for the rows about the ARN's region field.
const OTHER_REGION: &str = "eu-central-1";

/// The emulator with [`TABLE`] in place and one item stored, so no row is refused for a
/// missing table or a missing item.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "one"}}}),
    )
    .await;
    router
}

/// An ARN of this service in this endpoint's region, with `resource` as its whole
/// resource field.
fn arn_with_resource(resource: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:{resource}")
}

/// Every DATA-plane operation whose request carries a `TableName`, with the rest of a
/// request that reaches the resolution of that name.
fn data_plane(arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetItem", json!({"TableName": arn, "Key": {"pk": {"S": "one"}}})),
        ("PutItem", json!({"TableName": arn, "Item": {"pk": {"S": "one"}}})),
        (
            "UpdateItem",
            json!({"TableName": arn, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "SET note = :n",
                   "ExpressionAttributeValues": {":n": {"S": "a"}}}),
        ),
        (
            "DeleteItem",
            json!({"TableName": arn, "Key": {"pk": {"S": "one"}}}),
        ),
        (
            "Query",
            json!({"TableName": arn, "KeyConditionExpression": "pk = :k",
                   "ExpressionAttributeValues": {":k": {"S": "one"}}}),
        ),
        ("Scan", json!({"TableName": arn})),
    ]
}

/// Every CONTROL-plane operation driven here for the contrast, in the same form.
fn control_plane(arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeTable", json!({"TableName": arn})),
        ("DescribeTimeToLive", json!({"TableName": arn})),
        (
            "CreateBackup",
            json!({"TableName": arn, "BackupName": "planes"}),
        ),
    ]
}

/// (case, the submitted `TableName`, the diagnosis its wrong field earns). The resource
/// type can be wrong in several ways, and the region is wrong when it is not the one this
/// endpoint serves.
fn wrong_field_rows() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "a stream ARN",
            arn_with_resource(&format!("stream/{TABLE}")),
            "Invalid resource type",
        ),
        (
            "a backup ARN",
            arn_with_resource(&format!("backup/{TABLE}")),
            "Invalid resource type",
        ),
        (
            "a mis-cased table type",
            arn_with_resource(&format!("Table/{TABLE}")),
            "Invalid resource type",
        ),
        (
            "an ARN with no resource type at all",
            arn_with_resource(TABLE),
            "Invalid resource type",
        ),
        (
            "a table ARN of another Region",
            format!("arn:aws:dynamodb:{OTHER_REGION}:{ACCOUNT}:table/{TABLE}"),
            "Invalid AWS region",
        ),
    ]
}

#[tokio::test]
async fn the_data_plane_states_an_arn_field_refusal_as_the_table_name_member_s_constraint() {
    let router = seeded().await;

    for (case, arn, diagnosis) in wrong_field_rows() {
        for (operation, request) in data_plane(&arn) {
            let expected = format!(
                "1 validation error detected: Value '{arn}' at 'tableName' failed to satisfy \
                 constraint: {diagnosis}"
            );
            let (status, answered) = call(&router, operation, request).await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} ({operation}): expected {expected:?}, got {status} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn the_control_plane_states_the_same_field_refusal_as_the_diagnosis_sentence() {
    let router = seeded().await;

    for (case, arn, diagnosis) in wrong_field_rows() {
        for (operation, request) in control_plane(&arn) {
            let expected = format!("1 validation error detected: {diagnosis} in '{arn}'");
            let (status, answered) = call(&router, operation, request).await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} ({operation}): expected {expected:?}, got {status} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn the_table_s_own_arn_still_addresses_it_on_every_data_plane_operation() {
    let router = seeded().await;
    let own = arn_with_resource(&format!("table/{TABLE}"));

    // Every data-plane row is SERVED when the ARN's fields are right, so the refusals
    // above are about the field that was wrong. The write rows run before the read rows
    // of the next operation, so each one is answered from the item the previous left.
    for (operation, request) in data_plane(&own) {
        let answered = ok(&router, operation, request).await;
        assert!(
            answered.is_object(),
            "case the table's own ARN ({operation}): expected it to be served, got {answered}"
        );
    }

    // And a write addressed by the ARN is read back by the bare NAME, so the ARN
    // addressed the very table the name does.
    ok(
        &router,
        "PutItem",
        json!({"TableName": &own, "Item": {"pk": {"S": "two"}}}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "two"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["pk"],
        json!({"S": "two"}),
        "case the bare name: expected the item the ARN-addressed write stored, got {read}"
    );
}
