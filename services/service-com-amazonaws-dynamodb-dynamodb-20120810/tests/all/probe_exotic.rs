// SPDX-License-Identifier: CC0-1.0

//! Where a filter's OPERANDS may be written, driven end to end.
//!
//! The rule pinned here: an operand of a condition is a `:placeholder`, a document path or
//! a `size` call over one, written on its own or inside the parentheses that GROUP what the
//! grammar reads at that position, while a path that reaches as deep into the item as the
//! document-path grammar allows is read like any other.
//!
//! The rows span both halves: the operand forms the grammar reads (a path into a map, into
//! a list, into a map inside a list, a `size` over each of them, and each of those grouped)
//! and the form it does not (a pair of parentheses around an operand's own pair, which
//! groups what is already grouped), the latter refused for those parentheses rather than
//! evaluated.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, message, ok};
use crate::probe::refusal;
use crate::table_items::create_active_table;

/// The table every row scans.
const TABLE: &str = "ScanFilterOperandPositions";

#[tokio::test]
async fn an_operand_is_a_value_a_path_or_a_size_over_one() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "k1"},
            "s": {"S": "beta"},
            "lst": {"L": [{"S": "beta"}, {"M": {"deep": {"S": "x"}}}]},
            "mp": {"M": {"deep": {"S": "beta"}, "num": {"N": "7"}}},
        }}),
    )
    .await;

    // The forms the grammar reads: each is evaluated against the item, selecting it or not.
    let served: Vec<(&str, Value, i64)> = vec![
        ("mp.num = :seven", json!({":seven": {"N": "7"}}), 1),
        ("attribute_exists(lst[1].deep)", json!({}), 1),
        ("contains(lst[1].deep, :x)", json!({":x": {"S": "x"}}), 1),
        ("size(mp.deep) = :four", json!({":four": {"N": "4"}}), 1),
        ("size(lst) = :two", json!({":two": {"N": "2"}}), 1),
        ("size(lst[0]) = :four", json!({":four": {"N": "4"}}), 1),
        (
            "size(s) BETWEEN :one AND :nine",
            json!({":one": {"N": "1"}, ":nine": {"N": "9"}}),
            1,
        ),
        ("size(s) IN (:four)", json!({":four": {"N": "4"}}), 1),
        ("attribute_type(mp.num, :tn)", json!({":tn": {"S": "N"}}), 1),
        ("begins_with(mp.deep, :b)", json!({":b": {"S": "be"}}), 1),
        ("size(mp) = :one", json!({":one": {"N": "1"}}), 0),
        // The same operands with the parentheses that group them: a pair around an operand
        // or around a call's argument states that operand itself.
        ("attribute_exists((s))", json!({}), 1),
        ("contains(s, (:c))", json!({":c": {"S": "bet"}}), 1),
        ("size((s)) = (:four)", json!({":four": {"N": "4"}}), 1),
    ];
    for (filter, values, selected) in served {
        let mut request = json!({"TableName": TABLE, "FilterExpression": filter});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let (status, answered) = call(&router, "Scan", request).await;
        assert_eq!(
            status,
            200,
            "case the filter {filter:?}: expected a served scan, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            answered["Count"],
            json!(selected),
            "case the filter {filter:?}: expected {selected} item(s) selected, got {answered}"
        );
    }

    // The form it does not read: a pair of parentheses around an operand's own pair, which
    // groups what is already grouped and says nothing about how the expression is read.
    let redundant = "The expression has redundant parentheses;";
    let refused: Vec<(&str, Value, &str)> = vec![
        ("attribute_exists(((s)))", json!({}), redundant),
        ("contains(s, ((:c)))", json!({":c": {"S": "bet"}}), redundant),
        (
            "size(((s))) = :four",
            json!({":four": {"N": "4"}}),
            redundant,
        ),
    ];
    for (filter, values, detail) in refused {
        let mut request = json!({"TableName": TABLE, "FilterExpression": filter});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let expected = format!("Invalid FilterExpression: {detail}");
        let stated = refusal(&router, filter, "Scan", request).await;
        assert_eq!(
            stated, expected,
            "case the filter {filter:?}: expected {expected:?}, got {stated:?}"
        );
    }
}
