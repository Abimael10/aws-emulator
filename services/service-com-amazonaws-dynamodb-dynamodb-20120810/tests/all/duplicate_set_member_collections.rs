// SPDX-License-Identifier: CC0-1.0

//! WHICH collection a duplicate-member refusal NAMES, over the set types that name one.
//!
//! The rule under test: a set value supplied outside an `Item` — a `Key`, a condition's
//! operand, an element of a `Transact*Items` slot — must hold each member once ("the values
//! within a set must be unique", `smithy_query(query="shape",
//! shape="com.amazonaws.dynamodb#AttributeValue")`), and one that does not is refused 400
//! `ValidationException` naming the collection DynamoDB had COLLECTED when it read the
//! repeat: the distinct members before it, in the order that set type orders its members,
//! followed by the member that duplicated one of them. A BINARY set orders by ascending
//! unsigned byte value — "For Binary, DynamoDB treats each byte of the binary data as
//! unsigned" (the `AttributeValueList` doc) — so the collection the message names is not the
//! order the request listed, and the duplicate is the one member named twice in it.
//!
//! The rows span what the rule generalises over: the set TYPE (`BS`, which names its members
//! with the type appended, `SS`, which names them plain, and `NS`, whose wording names none),
//! how many distinct members precede the repeat, whether the request listed them in byte
//! order already, which member is the repeated one, and the member byte values themselves.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key, so a set is never a key of the
/// schema and the refusal is the set rule's rather than a key-type mismatch.
const TABLE: &str = "DuplicateSetMembers";

/// The envelope and fixed clause every one of these refusals opens with.
const CLAUSE: &str = "1 validation error detected: One or more parameter values were invalid: \
                      Input collection ";

/// One row: the set a `Key` carries, and the collection the message must name after
/// [`CLAUSE`] — the members as that type renders them, and the type tag a binary set appends.
struct Row {
    case: &'static str,
    value: Value,
    collection: &'static str,
}

/// The rows. A binary member is written as the base64 the wire carries, with the byte it
/// stands for named in the case.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a binary set listing 0x7a, 0x01, 0x01 — out of byte order",
            value: json!({"BS": ["eg==", "AQ==", "AQ=="]}),
            collection: "[\"AQ==\",\"eg==\",\"AQ==\"]of type BS ",
        },
        Row {
            case: "the same members already in byte order",
            value: json!({"BS": ["AQ==", "eg==", "AQ=="]}),
            collection: "[\"AQ==\",\"eg==\",\"AQ==\"]of type BS ",
        },
        Row {
            case: "a binary set repeating its first member immediately",
            value: json!({"BS": ["eg==", "eg=="]}),
            collection: "[\"eg==\",\"eg==\"]of type BS ",
        },
        Row {
            case: "a binary set of four members whose last repeats the second",
            // 0x03, 0x02, 0x01, 0x02 — collected as 0x01, 0x02, 0x03, then the repeat.
            value: json!({"BS": ["Aw==", "Ag==", "AQ==", "Ag=="]}),
            collection: "[\"AQ==\",\"Ag==\",\"Aw==\",\"Ag==\"]of type BS ",
        },
        Row {
            case: "a binary set whose members differ in length",
            // 0x7a7a, 0x7a, 0x7a — the shorter member sorts first.
            value: json!({"BS": ["eno=", "eg==", "eg=="]}),
            collection: "[\"eg==\",\"eno=\",\"eg==\"]of type BS ",
        },
        Row {
            case: "a string set repeating a member",
            value: json!({"SS": ["b", "b"]}),
            collection: "[\"b\", \"b\"] ",
        },
        Row {
            case: "a string set repeating its first member last",
            value: json!({"SS": ["a", "b", "a"]}),
            collection: "[\"a\", \"b\", \"a\"] ",
        },
        Row {
            // The number-set wording names no member at all, so the collection is empty.
            case: "a number set repeating a member spelled differently",
            value: json!({"NS": ["1", "1.0"]}),
            collection: "",
        },
    ]
}

/// RULE: the collection the refusal names is the one collected before the repeat, in that
/// type's own order, with the repeat appended.
#[tokio::test]
async fn a_duplicate_member_refusal_names_the_collection_it_collected() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    for row in rows() {
        let expected = format!("{CLAUSE}{}contains duplicates.", row.collection);
        let (status, body) = call(
            &router,
            "DeleteItem",
            json!({"TableName": TABLE, "Key": {"pk": row.value.clone()}}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", expected.as_str()),
            "case {:?}: expected {expected:?}, got {status} with {body}",
            row.case
        );
    }
}

/// RULE, over the other surfaces a set arrives through outside an `Item`: the same set earns
/// the same collection wherever the request carried it.
#[tokio::test]
async fn the_same_collection_is_named_wherever_the_set_arrived() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // The row whose listing order differs from the collected order, so each surface's
    // message is checked against the ORDER and not only against the members.
    let out_of_order = json!({"BS": ["eg==", "AQ==", "AQ=="]});
    let expected = format!("{CLAUSE}[\"AQ==\",\"eg==\",\"AQ==\"]of type BS contains duplicates.");

    for (case, operation, request) in [
        (
            "a GetItem key",
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": out_of_order.clone()}}),
        ),
        (
            "an UpdateItem condition operand",
            "UpdateItem",
            json!({
                "TableName": TABLE,
                "Key": {"pk": {"S": "present"}},
                "ConditionExpression": "held = :dup",
                "ExpressionAttributeValues": {":dup": out_of_order.clone()},
            }),
        ),
        (
            "a Query filter operand",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :p",
                "FilterExpression": "held = :dup",
                "ExpressionAttributeValues": {
                    ":p": {"S": "present"},
                    ":dup": out_of_order.clone(),
                },
            }),
        ),
    ] {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the set rule to refuse the call, got {status} with {body}"
        );
        let stated = message(&body);
        assert!(
            stated.ends_with("[\"AQ==\",\"eg==\",\"AQ==\"]of type BS contains duplicates.")
                && stated.contains("One or more parameter values were invalid: Input collection "),
            "case {case:?}: expected the collected collection in {expected:?}, got {stated:?}"
        );
    }
}
