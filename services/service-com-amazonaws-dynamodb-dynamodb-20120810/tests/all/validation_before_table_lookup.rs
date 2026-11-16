// SPDX-License-Identifier: CC0-1.0

//! WHICH side of the table lookup a data-plane rule is settled on, driven end to end.
//!
//! RULE under test: a rule that depends on NO table is settled before the table is looked
//! up, so a request that cannot be served however the store looks is refused as the
//! malformed request it is; a rule that is a QUOTA on what a table holds is asked of a
//! table that is there, so the same request against a table this account does not hold is
//! the missing table.
//!
//! The rows differ along the rule (two identical keys in one batched read's `Keys` list,
//! the documented value rules, the documented nesting ceiling, the documented 400 KB item
//! ceiling), the operation that carries it (`BatchGetItem`, `PutItem`, `BatchWriteItem`),
//! and whether the table the request names exists — which is the axis the rule is about,
//! so every row is driven BOTH ways.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table the "present" half of each row addresses.
const TABLE: &str = "ValidationBeforeLookup";

/// A table name no request here creates, which is what makes the existence check decide.
const ABSENT: &str = "NoSuchValidationTable";

/// AWS's fixed data-plane sentence for a table that is not there.
const NOT_FOUND: &str = "Requested resource not found";

/// AWS's fixed sentence for one entry of a batched read asking for one key twice.
const DUPLICATE_KEYS: &str = "Provided list of item keys contains duplicates";

/// AWS's fixed sentence for a request that carried an item past the documented ceiling.
const ITEM_TOO_BIG: &str = "Item size has exceeded the maximum allowed size";

/// AWS's fixed sentence for the documented nesting ceiling, stated on its own because the
/// item of one `RequestItems` entry is a member of that entry.
const NESTING: &str = "Nesting Levels have exceeded supported limits: Attributes in the item \
                       have nested levels beyond supported limit";

fn key(pk: &str) -> Value {
    json!({"pk": {"S": pk}})
}

/// A value nesting `levels` containers deep, each level a one-element list.
fn nested(levels: usize) -> Value {
    let mut value = json!({"S": "deep"});
    for _ in 0..levels {
        value = json!({"L": [value]});
    }
    value
}

/// An item past the documented 400 KB ceiling, built from `attributes` values each well
/// INSIDE it: what crosses the ceiling is the item, which is the quota this is about.
fn oversized_from(pk: &str, attributes: usize) -> Value {
    let mut item = json!({"pk": {"S": pk}});
    for at in 0..attributes {
        item[format!("part{at}")] = json!({"S": "f".repeat(150 * 1024)});
    }
    item
}

/// The smallest such item: three parts, so the total is 450 KB.
fn oversized(pk: &str) -> Value {
    oversized_from(pk, 3)
}

async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "item-1"}}}),
    )
    .await;
    router
}

/// The status, code and message one call answers.
async fn answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, String, String) {
    let (status, parsed) = call(router, operation, body).await;
    (status, error_code(&parsed), message(&parsed))
}

/// How one row below builds its call over a table name: the name in, the operation and its
/// body out, so the SAME call can be driven against a table that is there and one that is
/// not.
type Over = fn(&str) -> (&'static str, Value);

/// One row: the rule, the call that carries it, and the whole message it must earn.
struct Row {
    case: &'static str,
    over: Over,
    expected: &'static str,
}

#[tokio::test]
async fn a_rule_the_request_settles_is_answered_whichever_table_it_named() {
    let router = seeded().await;

    let rows: Vec<Row> = vec![
        Row {
            case: "one entry of a batched read asking for one key twice",
            over: |table| {
                (
                    "BatchGetItem",
                    json!({"RequestItems": {table: {"Keys": [key("item-1"), key("item-1")]}}}),
                )
            },
            expected: DUPLICATE_KEYS,
        },
        Row {
            case: "one entry of a batched read asking for three keys, two of them the same",
            over: |table| {
                (
                    "BatchGetItem",
                    json!({"RequestItems": {table: {
                        "Keys": [key("item-1"), key("item-2"), key("item-2")]}}}),
                )
            },
            expected: DUPLICATE_KEYS,
        },
        Row {
            case: "an item of a batched write nesting one level past the ceiling",
            over: |table| {
                (
                    "BatchWriteItem",
                    json!({"RequestItems": {table: [{"PutRequest": {"Item":
                        {"pk": {"S": "deep-1"}, "tree": nested(33)}}}]}}),
                )
            },
            expected: NESTING,
        },
        Row {
            case: "an item of a batched write past the 400 KB ceiling",
            over: |table| {
                (
                    "BatchWriteItem",
                    json!({"RequestItems": {table: [{"PutRequest":
                        {"Item": oversized("big-1")}}]}}),
                )
            },
            expected: ITEM_TOO_BIG,
        },
    ];

    for row in &rows {
        let (case, expected) = (row.case, row.expected);
        for table in [TABLE, ABSENT] {
            let (operation, body) = (row.over)(table);
            let (status, code, said) = answered(&router, operation, body).await;
            assert_eq!(
                (status, code.as_str(), said.as_str()),
                (400, "ValidationException", expected),
                "case {case:?} against {table:?}: a rule the request settles is answered \
                 whichever table it named, got {status} {code} {said:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_quota_on_what_a_table_holds_needs_the_table() {
    let router = seeded().await;

    // The documented 400 KB ceiling is a quota on what a TABLE can hold, so a single-item
    // write of an oversized item is told the ceiling where the table is there and told the
    // table is not where it is not. The rows differ in how far past the ceiling the item
    // sits, because the answer is the ceiling and not the figure.
    for (case, item) in [
        ("just past the ceiling", oversized("big-2")),
        ("well past the ceiling", oversized_from("big-3", 6)),
    ] {
        let (status, code, said) = answered(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": item.clone()}),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), said.as_str()),
            (400, "ValidationException", ITEM_TOO_BIG),
            "case {case:?} against the table that holds it: expected the item ceiling, got \
             {status} {code} {said:?}"
        );

        let (status, code, said) = answered(
            &router,
            "PutItem",
            json!({"TableName": ABSENT, "Item": item}),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), said.as_str()),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?} against the table this account does not hold: expected the missing \
             table, got {status} {code} {said:?}"
        );
    }

    // The CONTROL: the same two operations with nothing wrong are served, so the refusals
    // above are these rules and not a blanket rejection.
    let read = ok(
        &router,
        "BatchGetItem",
        json!({"RequestItems": {TABLE: {"Keys": [key("item-1"), key("item-2")]}}}),
    )
    .await;
    assert_eq!(
        read["Responses"][TABLE],
        json!([{"pk": {"S": "item-1"}}]),
        "case \"two distinct keys\": expected the one stored item, got {read}"
    );
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "item-2"}, "body": {"S": "small"}}}),
    )
    .await;
    let stored = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key("item-2")}),
    )
    .await;
    assert_eq!(
        stored["Item"]["body"],
        json!({"S": "small"}),
        "case \"an item inside the ceiling\": expected the written value, got {stored}"
    );
}
