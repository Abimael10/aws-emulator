// SPDX-License-Identifier: CC0-1.0

//! Which VALUES a read's filter may compare against, driven end to end.
//!
//! The rule pinned here: a value a filter binds through `ExpressionAttributeValues` is held
//! to the rules of its own TYPE and to nothing else — the same rules a value a write stores
//! is held to — so every well-formed `AttributeValue` is served whatever its type and
//! compared AS that type ("If an item contains an AttributeValue element of a different
//! type than the one provided in the request, the value does not match" —
//! `Condition$ComparisonOperator`), and only a value that is no value of its own type is
//! refused.
//!
//! The rows span the types an `AttributeValue` can carry, both ends of the documented
//! numeric domain, the empty forms AWS admits for a non-key attribute (an empty string, an
//! empty binary, an empty list, an empty map) and the one malformed form: `NULL` is a
//! marker, so it carries the value `true` and nothing else. Each row states what the two
//! filters it drives SELECT with it, so the row pins the comparison and not only the
//! value's admission.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, message, ok};
use crate::probe::refusal;
use crate::table_items::create_active_table;

/// The table every row scans.
const TABLE: &str = "ScanFilterValueShapes";

#[tokio::test]
async fn a_filter_compares_against_any_well_formed_attribute_value() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "k1"}, "s": {"S": "beta"},
                                            "n": {"N": "3"}}}),
    )
    .await;

    // Each row: the value, how many of the one seeded item `s = :v` selects with it, and
    // how many `contains(s, :v)` does. The item holds `s` as "beta", so only the row whose
    // value IS that text is equal to it and only a substring of it is contained in it; a
    // value of any other type matches neither.
    //
    // Source of the numeric bounds: "Number ... 38 significant digits, exponent range -130
    // to +125" (the Developer Guide's data-type reference for `AttributeValue$N`), so the
    // rows carry both published bounds.
    let served: Vec<(&str, Value, i64, i64)> = vec![
        ("the stored text itself", json!({"S": "beta"}), 1, 1),
        ("a substring of it", json!({"S": "bet"}), 0, 1),
        ("an empty string", json!({"S": ""}), 0, 1),
        ("an empty binary", json!({"B": ""}), 0, 0),
        ("a null marker", json!({"NULL": true}), 0, 0),
        ("a boolean", json!({"BOOL": false}), 0, 0),
        ("an empty list", json!({"L": []}), 0, 0),
        ("an empty map", json!({"M": {}}), 0, 0),
        ("a one-member string set", json!({"SS": ["a"]}), 0, 0),
        ("a number set", json!({"NS": ["1", "2"]}), 0, 0),
        ("a zero", json!({"N": "0"}), 0, 0),
        ("a trailing-zero decimal", json!({"N": "1.0"}), 0, 0),
        ("38 significant digits", json!({"N": "1".repeat(38)}), 0, 0),
        (
            "the top of the exponent range",
            json!({"N": "9.9999999999999999999999999999999999999e125"}),
            0,
            0,
        ),
        (
            "the bottom of the exponent range",
            json!({"N": "1e-130"}),
            0,
            0,
        ),
        ("a nested list", json!({"L": [{"L": [{"S": "x"}]}]}), 0, 0),
        (
            "a nested map",
            json!({"M": {"a": {"M": {"b": {"S": "x"}}}}}),
            0,
            0,
        ),
    ];
    for (case, value, equal, contained) in served {
        for (filter, selected) in [("s = :v", equal), ("contains(s, :v)", contained)] {
            let (status, answered) = call(
                &router,
                "Scan",
                json!({"TableName": TABLE, "FilterExpression": filter,
                       "ExpressionAttributeValues": {":v": value.clone()}}),
            )
            .await;
            assert_eq!(
                status,
                200,
                "case {case:?} in {filter:?}: expected a served scan, got {status} with {}",
                message(&answered)
            );
            assert_eq!(
                (answered["Count"].clone(), answered["ScannedCount"].clone()),
                (json!(selected), json!(1)),
                "case {case:?} in {filter:?}: expected {selected} of the one scanned item \
                 selected, got {answered}"
            );
        }
    }

    // The CONTROL: a value that is no value of its own type is refused, which is what
    // makes the rows above an admission rule and not a blanket acceptance.
    let stated = refusal(
        &router,
        "a null marker carrying false",
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "s = :v",
               "ExpressionAttributeValues": {":v": {"NULL": false}}}),
    )
    .await;
    assert_eq!(
        stated,
        "One or more parameter values were invalid: Null attribute value types must have the \
         value of true",
        "case a null marker carrying false: expected the refusal for it, got {stated:?}"
    );
}
