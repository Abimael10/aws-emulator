// SPDX-License-Identifier: CC0-1.0

//! WHICH FRAME a refusal about a FIELD of the ARN one TRANSACTION ELEMENT's `TableName`
//! carried is stated in, driven end to end over this service's own wire protocol.
//!
//! RULE under test: a `TableName` may be spelled as the table's ARN ("You can also provide
//! the Amazon Resource Name (ARN) of the table in this parameter" —
//! `smithy_query(query="doc",shape="com.amazonaws.dynamodb#GetInput",member="TableName")`),
//! and an endpoint serves ONE Region, so an ARN naming another Region names nothing that
//! endpoint can reach. `TransactGetItems` and `TransactWriteItems` read and write the
//! ITEMS of the tables their elements address, so — like every other operation of that
//! plane — each states such a diagnosis as the carrying member's own CONSTRAINT CLAUSE,
//! with the ARN the request sent echoed; and because the member belongs to ONE ELEMENT of
//! a list of up to 100, the clause names its FULL path: the list, the element's 1-based
//! position in it, the collection step into that element, the action it states and the
//! leaf member.
//!
//! The rows differ along what the rule generalises over: the OPERATION, the ACTION an
//! element states (`TransactWriteItems` models four of them), the POSITION of the
//! offending element in the list, and the FIELD of the ARN that is wrong (its Region, and
//! its resource type for the contrast). The last test drives the table's OWN ARN through
//! the same elements, so the refusals are about the field and not about an ARN in the
//! member.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok, REGION};
use crate::table_items::create_active_table;

/// The table every element addresses.
const TABLE: &str = "TxnFrames";

/// The account id every generated emulator presents (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// A Region this endpoint does not serve, for the rows about the ARN's Region field.
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

/// One `TransactWriteItems` element per action the model declares
/// (`smithy_query(query="shape",shape="com.amazonaws.dynamodb#TransactWriteItem")`:
/// `ConditionCheck`, `Put`, `Delete`, `Update`), addressing `table`, with the member name
/// the request validator spells that action by.
fn write_elements(table: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "put",
            json!({"Put": {"TableName": table, "Item": {"pk": {"S": "one"}}}}),
        ),
        (
            "delete",
            json!({"Delete": {"TableName": table, "Key": {"pk": {"S": "one"}}}}),
        ),
        (
            "update",
            json!({"Update": {"TableName": table, "Key": {"pk": {"S": "one"}},
                              "UpdateExpression": "SET note = :n",
                              "ExpressionAttributeValues": {":n": {"S": "a"}}}}),
        ),
        (
            "conditionCheck",
            json!({"ConditionCheck": {"TableName": table, "Key": {"pk": {"S": "one"}},
                                      "ConditionExpression": "attribute_exists(pk)"}}),
        ),
    ]
}

/// A `TransactGetItems` element addressing `table`.
fn get_element(table: &str) -> Value {
    json!({"Get": {"TableName": table, "Key": {"pk": {"S": "one"}}}})
}

/// The clause a refusal about the ARN `value` at `path` states.
fn clause(path: &str, value: &str, diagnosis: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         {diagnosis}"
    )
}

/// (case, the submitted `TableName`, the diagnosis its wrong field earns).
fn wrong_field_rows() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "a table ARN of another Region",
            format!("arn:aws:dynamodb:{OTHER_REGION}:{ACCOUNT}:table/{TABLE}"),
            "Invalid AWS region",
        ),
        (
            "a stream ARN of this Region",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:stream/{TABLE}"),
            "Invalid resource type",
        ),
    ]
}

#[tokio::test]
async fn a_transactional_get_names_the_element_whose_arn_field_is_wrong() {
    let router = seeded().await;

    for (case, arn, diagnosis) in wrong_field_rows() {
        // The offending element at each position of a list that also carries a good one,
        // so the path's position is read off the list rather than assumed to be the first.
        for position in [1_usize, 2] {
            let mut elements = vec![get_element(TABLE), get_element(TABLE)];
            elements[position - 1] = get_element(&arn);
            let expected = clause(
                &format!("transactItems.{position}.member.get.tableName"),
                &arn,
                diagnosis,
            );
            let (status, answered) = call(
                &router,
                "TransactGetItems",
                json!({"TransactItems": elements}),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} at position {position}: expected {expected:?}, got {status} with \
                 {answered}"
            );
        }
    }
}

#[tokio::test]
async fn a_transactional_write_names_the_element_and_the_action_it_states() {
    let router = seeded().await;

    for (case, arn, diagnosis) in wrong_field_rows() {
        for (member, element) in write_elements(&arn) {
            // Position 2, behind an element of the table itself, so the clause's path is
            // about the element that carried the ARN and not about the first one.
            let elements = vec![
                json!({"Put": {"TableName": TABLE, "Item": {"pk": {"S": "two"}}}}),
                element,
            ];
            let expected = clause(
                &format!("transactItems.2.member.{member}.tableName"),
                &arn,
                diagnosis,
            );
            let (status, answered) = call(
                &router,
                "TransactWriteItems",
                json!({"TransactItems": elements}),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} ({member}): expected {expected:?}, got {status} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn the_tables_own_arn_still_addresses_it_from_every_element() {
    let router = seeded().await;
    let own = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}");

    // Every element is SERVED when the ARN's fields are right, so the refusals above are
    // about the field that was wrong.
    let answered = ok(
        &router,
        "TransactGetItems",
        json!({"TransactItems": [get_element(&own)]}),
    )
    .await;
    assert_eq!(
        answered["Responses"][0]["Item"]["pk"],
        json!({"S": "one"}),
        "case the table's own ARN (TransactGetItems): expected the stored item, got {answered}"
    );
    for (member, element) in write_elements(&own) {
        let answered = ok(
            &router,
            "TransactWriteItems",
            json!({"TransactItems": [element]}),
        )
        .await;
        assert!(
            answered.is_object() || answered.is_null(),
            "case the table's own ARN ({member}): expected it to be served, got {answered}"
        );
        // The item the delete removed is put back, so the next element of the loop finds
        // the item its action needs.
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "one"}}}),
        )
        .await;
    }
}
