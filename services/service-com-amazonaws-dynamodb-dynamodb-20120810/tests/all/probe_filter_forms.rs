// SPDX-License-Identifier: CC0-1.0

//! Which FILTER FORMS a `Scan` serves, driven end to end.
//!
//! The rule pinned here: a condition function's operand position that the reference calls
//! an OPERAND — "contains (path, operand)", "begins_with (path, substr)" — takes a document
//! path as readily as a `:placeholder`, and the call is then evaluated against the item
//! rather than refused. The table below spans that rule's surface: both functions, the
//! attribute types a substring or prefix search is defined over, a path that reaches inside
//! a map or a list, the `size` operand, the keyword casing an expression may be written in,
//! and the aliased spelling of every one of them.
//!
//! Each row is a filter the service must SERVE: a form refused here is a correct query a
//! client cannot run.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, message, ok};
use crate::table_items::create_active_table;

/// The table every row scans, holding one item whose attributes cover the types a filter
/// function reads: text, a number, a string set, a list, a map and binary.
const TABLE: &str = "ScanFilterOperandForms";

/// The emulator with [`TABLE`] and that one item in place.
async fn seeded() -> smithy_server_runtime::MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "k1"},
            "s": {"S": "beta"},
            "peer": {"S": "beta"},
            "n": {"N": "3"},
            "tags": {"SS": ["red", "beta"]},
            "lst": {"L": [{"S": "beta"}, {"N": "1"}]},
            "mp": {"M": {"deep": {"S": "beta"}}},
            "bin": {"B": ::aws_smithy_types::base64::encode([1u8, 2])},
        }}),
    )
    .await;
    router
}

#[tokio::test]
async fn a_scan_serves_every_filter_form_whose_operands_its_functions_take() {
    let router = seeded().await;

    // Each row: the filter, the substitution tables it needs, and how many of the one
    // seeded item it selects — 1 when the condition holds for it and 0 when it does not,
    // which is still a SERVED scan.
    let rows: Vec<(&str, Value, Value, i64)> = vec![
        ("attribute_exists(s)", json!({}), json!({}), 1),
        ("attribute_not_exists(zzz)", json!({}), json!({}), 1),
        (
            "attribute_type(s, :ts)",
            json!({":ts": {"S": "S"}}),
            json!({}),
            1,
        ),
        (
            "attribute_type(tags, :tss)",
            json!({":tss": {"S": "SS"}}),
            json!({}),
            1,
        ),
        (
            "begins_with(s, :p)",
            json!({":p": {"S": "bet"}}),
            json!({}),
            1,
        ),
        (
            "begins_with(bin, :pb)",
            json!({":pb": {"B": ::aws_smithy_types::base64::encode([1u8])}}),
            json!({}),
            1,
        ),
        (
            "contains(tags, :c)",
            json!({":c": {"S": "beta"}}),
            json!({}),
            1,
        ),
        (
            "contains(lst, :c)",
            json!({":c": {"S": "beta"}}),
            json!({}),
            1,
        ),
        // The operand read from the ITEM, which is the rule this suite is about: the
        // seeded item holds `s` and `peer` as the same text.
        ("contains(s, peer)", json!({}), json!({}), 1),
        ("begins_with(s, peer)", json!({}), json!({}), 1),
        ("contains(tags, peer)", json!({}), json!({}), 1),
        ("contains(s, pk)", json!({}), json!({}), 0),
        ("contains(bin, peer)", json!({}), json!({}), 0),
        // The same operand forms behind aliases, and over a path that reaches inside a map
        // or a list.
        (
            "contains(#a, #b)",
            json!({}),
            json!({"#a": "s", "#b": "peer"}),
            1,
        ),
        (
            "begins_with(#a, #b)",
            json!({}),
            json!({"#a": "s", "#b": "peer"}),
            1,
        ),
        (
            "contains(#m.#d, :c)",
            json!({":c": {"S": "bet"}}),
            json!({"#m": "mp", "#d": "deep"}),
            1,
        ),
        (
            "begins_with(mp.deep, peer)",
            json!({}),
            json!({}),
            1,
        ),
        ("contains(lst[0], peer)", json!({}), json!({}), 1),
        // `size` is an operand, so it stands wherever one does.
        ("n = size(s)", json!({}), json!({}), 0),
        (
            "size(s) = :four",
            json!({":four": {"N": "4"}}),
            json!({}),
            1,
        ),
        ("size(s) = size(peer)", json!({}), json!({}), 1),
        // The combining keywords are not case-sensitive the way the function names are.
        ("attribute_exists(s) and attribute_exists(n)", json!({}), json!({}), 1),
        ("not attribute_exists(zzz)", json!({}), json!({}), 1),
        (
            "s between :lo and :hi",
            json!({":lo": {"S": "a"}, ":hi": {"S": "z"}}),
            json!({}),
            1,
        ),
        (
            "s in (:c, :p)",
            json!({":c": {"S": "beta"}, ":p": {"S": "bet"}}),
            json!({}),
            1,
        ),
    ];

    for (filter, values, names, selected) in rows {
        let mut request = json!({"TableName": TABLE, "FilterExpression": filter});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        if !names.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeNames"] = names;
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
        assert_eq!(
            answered["ScannedCount"],
            json!(1),
            "case the filter {filter:?}: expected the one stored item scanned, got {answered}"
        );
    }
}
