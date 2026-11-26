// SPDX-License-Identifier: CC0-1.0

//! What the LEGACY `AttributeUpdates` map does to a stored item, action by action.
//!
//! The rule under test: `UpdateItem` applies each entry of that map to the attribute the
//! entry names, by the action the entry states — "PUT - Adds the specified attribute to the
//! item. If the attribute already exists, it is replaced by the new value", "DELETE - If no
//! value is specified, the attribute and its value are removed from the item ... If a set of
//! values is specified, then those values are subtracted from the old set", and "ADD - If the
//! attribute does not already exist, then the attribute and its values are added to the item.
//! If the attribute does exist, then the behavior of ADD depends on the data type of the
//! attribute" (`smithy_query(query="doc", shape="com.amazonaws.dynamodb#AttributeValueUpdate",
//! member="Action")`). The map states no expression, so no refusal about an update expression
//! is reachable through it, and the data types `ADD` combines are the attribute's own: a
//! number is summed, a set of the same primitive type is unioned, and a LIST is concatenated
//! (recorded behaviour, which the member's own doc leaves to "the data type of the
//! attribute"), while a type `ADD` cannot combine with what is held is refused.
//!
//! The rows span what the rule generalises over: the three actions, the stored types each
//! applies to (number, the three set types, a list, a map), whether the entry carries a value
//! at all, and `ReturnValues` `UPDATED_NEW` and `UPDATED_OLD`, which report the attributes
//! the entries touched. Every change is read back through `GetItem`.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "LegacyAttributeUpdates";

/// AWS's fixed sentence for an update operand of a type the stored attribute cannot be
/// combined with.
const OPERAND_TYPE: &str = "An operand in the update expression has an incorrect data type";

/// The key of the item a row works on, one per row so no two rows read what the other wrote.
fn key(of: &str) -> Value {
    json!({"pk": {"S": of}})
}

/// Seed `of`'s item with a number, a string set, a list and a map — one attribute of each
/// type the actions below combine with.
async fn seeded(router: &smithy_server_runtime::MultiServiceRouter, of: &str) {
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": of},
            "n": {"N": "10"},
            "ss": {"SS": ["a", "b"]},
            "l": {"L": [{"S": "one"}]},
            "m": {"M": {"held": {"S": "yes"}}},
        }}),
    )
    .await;
}

/// The stored item as a `ConsistentRead` `GetItem` reports it.
async fn stored(router: &smithy_server_runtime::MultiServiceRouter, of: &str) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key(of), "ConsistentRead": true}),
    )
    .await;
    read["Item"].clone()
}

/// One row: the `AttributeUpdates` map, what the stored item must hold for the attributes it
/// touched afterwards, and what `UPDATED_NEW` must report.
struct Row {
    case: &'static str,
    of: &'static str,
    updates: Value,
    /// `(attribute, value)` pairs the stored item must carry after the update; a `null`
    /// value means the attribute must be gone.
    stored: Vec<(&'static str, Value)>,
    /// What `ReturnValues=UPDATED_NEW` must answer.
    updated_new: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "ADD unions a string set into the stored one",
            of: "add-set",
            updates: json!({"ss": {"Action": "ADD", "Value": {"SS": ["b", "c"]}}}),
            stored: vec![("ss", json!({"SS": ["a", "b", "c"]}))],
            updated_new: json!({"ss": {"SS": ["a", "b", "c"]}}),
        },
        Row {
            case: "ADD sums a number into the stored one",
            of: "add-number",
            updates: json!({"n": {"Action": "ADD", "Value": {"N": "5"}}}),
            stored: vec![("n", json!({"N": "15"}))],
            updated_new: json!({"n": {"N": "15"}}),
        },
        Row {
            case: "ADD appends a one-element list to the stored list",
            of: "add-list",
            updates: json!({"l": {"Action": "ADD", "Value": {"L": [{"S": "two"}]}}}),
            stored: vec![("l", json!({"L": [{"S": "one"}, {"S": "two"}]}))],
            updated_new: json!({"l": {"L": [{"S": "one"}, {"S": "two"}]}}),
        },
        Row {
            case: "ADD on an attribute that is not there yet stores the value itself",
            of: "add-fresh",
            updates: json!({"fresh": {"Action": "ADD", "Value": {"NS": ["7"]}}}),
            stored: vec![("fresh", json!({"NS": ["7"]}))],
            updated_new: json!({"fresh": {"NS": ["7"]}}),
        },
        Row {
            case: "DELETE with a set value subtracts those members",
            of: "delete-member",
            updates: json!({"ss": {"Action": "DELETE", "Value": {"SS": ["a"]}}}),
            stored: vec![("ss", json!({"SS": ["b"]}))],
            updated_new: json!({"ss": {"SS": ["b"]}}),
        },
        Row {
            case: "DELETE with no value removes the attribute",
            of: "delete-whole",
            updates: json!({"ss": {"Action": "DELETE"}}),
            stored: vec![("ss", Value::Null)],
            // An `UPDATED_*` projection whose changed path addresses nothing in the item is
            // reported as no `Attributes` member at all.
            updated_new: Value::Null,
        },
        Row {
            case: "PUT replaces a stored map wholesale",
            of: "put-map",
            updates: json!({"m": {"Action": "PUT", "Value": {"M": {"other": {"N": "1"}}}}}),
            stored: vec![("m", json!({"M": {"other": {"N": "1"}}}))],
            updated_new: json!({"m": {"M": {"other": {"N": "1"}}}}),
        },
        Row {
            case: "PUT with no Action stated at all defaults to PUT",
            of: "put-default",
            updates: json!({"m": {"Value": {"S": "flat"}}}),
            stored: vec![("m", json!({"S": "flat"}))],
            updated_new: json!({"m": {"S": "flat"}}),
        },
        Row {
            case: "three actions in one map, each applied to its own attribute",
            of: "three-actions",
            updates: json!({
                "n": {"Action": "ADD", "Value": {"N": "1"}},
                "ss": {"Action": "DELETE", "Value": {"SS": ["b"]}},
                "m": {"Action": "PUT", "Value": {"BOOL": true}},
            }),
            stored: vec![
                ("n", json!({"N": "11"})),
                ("ss", json!({"SS": ["a"]})),
                ("m", json!({"BOOL": true})),
            ],
            updated_new: json!({
                "n": {"N": "11"},
                "ss": {"SS": ["a"]},
                "m": {"BOOL": true},
            }),
        },
    ]
}

/// RULE: every action of the legacy map is applied to the attribute it names, reported
/// through `UPDATED_NEW`, and reads back from the store.
#[tokio::test]
async fn each_legacy_action_is_applied_and_reported() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for row in rows() {
        seeded(&router, row.of).await;
        let (status, body) = call(
            &router,
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": key(row.of),
                "AttributeUpdates": row.updates.clone(),
                "ReturnValues": "UPDATED_NEW",
            }),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case {:?}: expected the legacy update to be applied, got {status} with {}",
            row.case,
            message(&body)
        );
        assert_eq!(
            body["Attributes"],
            row.updated_new,
            "case {:?}: expected UPDATED_NEW to report {:?}, got {:?}",
            row.case,
            row.updated_new,
            body["Attributes"]
        );

        let item = stored(&router, row.of).await;
        for (attribute, expected) in &row.stored {
            let held = item.get(*attribute).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &held, expected,
                "case {:?}: expected {attribute:?} to read back as {expected:?}, got {held:?}",
                row.case
            );
        }
    }
}

/// RULE, the other half of `ReturnValues`: `UPDATED_OLD` reports the touched attributes as
/// they stood BEFORE the change.
#[tokio::test]
async fn updated_old_reports_the_touched_attributes_as_they_stood() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // Each row: the map, and what the attributes it touches held before it ran.
    for (case, of, updates, expected) in [
        (
            "ADD onto a stored set",
            "old-add-set",
            json!({"ss": {"Action": "ADD", "Value": {"SS": ["c"]}}}),
            json!({"ss": {"SS": ["a", "b"]}}),
        ),
        (
            "ADD onto a stored number",
            "old-add-number",
            json!({"n": {"Action": "ADD", "Value": {"N": "2"}}}),
            json!({"n": {"N": "10"}}),
        ),
        (
            "DELETE of a whole attribute",
            "old-delete",
            json!({"l": {"Action": "DELETE"}}),
            json!({"l": {"L": [{"S": "one"}]}}),
        ),
        (
            "PUT over a stored map",
            "old-put",
            json!({"m": {"Action": "PUT", "Value": {"S": "replaced"}}}),
            json!({"m": {"M": {"held": {"S": "yes"}}}}),
        ),
    ] {
        seeded(&router, of).await;
        let body = ok(
            &router,
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": key(of),
                "AttributeUpdates": updates,
                "ReturnValues": "UPDATED_OLD",
            }),
        )
        .await;
        assert_eq!(
            body["Attributes"], expected,
            "case {case:?}: expected UPDATED_OLD to report {expected:?}, got {:?}",
            body["Attributes"]
        );
    }
}

/// RULE: a type `ADD` cannot combine with what the attribute HOLDS is still refused, so the
/// list form above is one more data type the action combines and not a dropped rule. The
/// refusal names neither operand, which is how this service words that class — told apart
/// from a VALUE of a type the action is not defined over at all, which is the parameter-value
/// refusal naming the action and that type (`tests/all/legacy_action_value_domains.rs`).
#[tokio::test]
async fn an_add_the_stored_type_cannot_combine_with_is_refused() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    seeded(&router, "mismatched").await;

    // Each row: the entry, and why the stored type cannot take it — a list onto a map, a
    // set onto a number, one set type onto another, and a number onto a list. Every value
    // here is one the action IS defined over, so what refuses each row is the type the item
    // holds at that attribute.
    for (case, updates) in [
        (
            "a list added onto a stored map",
            json!({"m": {"Action": "ADD", "Value": {"L": [{"S": "x"}]}}}),
        ),
        (
            "a string set added onto a stored number",
            json!({"n": {"Action": "ADD", "Value": {"SS": ["x"]}}}),
        ),
        (
            "a number set added onto a stored string set",
            json!({"ss": {"Action": "ADD", "Value": {"NS": ["1"]}}}),
        ),
        (
            "a number added onto a stored list",
            json!({"l": {"Action": "ADD", "Value": {"N": "1"}}}),
        ),
    ] {
        let (status, body) = call(
            &router,
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": key("mismatched"),
                "AttributeUpdates": updates,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", OPERAND_TYPE),
            "case {case:?}: expected the operand-type refusal, got {status} with {body}"
        );
        // The refused update wrote nothing: the seeded item is as it was.
        let item = stored(&router, "mismatched").await;
        assert_eq!(
            item["n"],
            json!({"N": "10"}),
            "case {case:?}: a refused update must leave the item where it was, got {item}"
        );
    }
}
