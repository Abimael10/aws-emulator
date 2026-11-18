// SPDX-License-Identifier: CC0-1.0

//! Four rules of the expression language, each asserted over the class it generalises
//! over rather than over one recorded expression.
//!
//! 1. A pair of parentheses groups what the grammar reads at the position it stands in.
//!    Where a comparison reads an OPERAND, a group states that operand — so
//!    `(size(a)) = :n` is the comparison it reads, and the group a condition stands in is
//!    still the condition it was. In a `SET` value the group states the WHOLE value, which
//!    is why an arithmetic operator written after one is where the chain the expression
//!    spells breaks, and the token the refusal names.
//! 2. A bracketed list index is spelled in single characters, so an index the lexer cannot
//!    read is refused at the CHARACTER where it stops being one, quoting that character
//!    with the one before and the one after it.
//! 3. The legacy `Condition$AttributeValueList` states no operand count, so an `IN` entry
//!    is compared against every item read however many values it lists; the documented
//!    100-value ceiling belongs to the `IN` OPERATOR of the expression language.
//! 4. An update operand whose type its operator cannot take is refused by WHERE that type
//!    came from: a type the request spelled is named back beside the operator, while a
//!    type read from the stored item earns AWS's fixed class sentence.
//!
//! Every call is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, message, ok};
use crate::probe::refusal;
use crate::table_items::create_active_table;

/// The table every row of this suite addresses.
const TABLE: &str = "ExpressionGroups";

/// AWS's fixed sentence for an update path that addresses no location of the item.
const INVALID_PATH: &str = "The document path provided in the update expression is invalid for \
                            update";

/// AWS's fixed sentence for an operand whose type is the STORED one: it names neither
/// operand.
const STORED_TYPE_MISMATCH: &str = "An operand in the update expression has an incorrect data \
                                    type";

/// One item carrying a value of each type a `size` call has a length for, plus a number,
/// under the partition key `pk`.
fn stored(pk: &str) -> Value {
    json!({
        "pk": {"S": pk},
        "s": {"S": "beta"},
        "b": {"B": "AQID"},
        "n": {"N": "7"},
        "ss": {"SS": ["a", "b"]},
        "l": {"L": [{"S": "x"}, {"S": "y"}]},
        "m": {"M": {"leaf": {"S": "x"}}},
    })
}

/// One emulator holding `TABLE` with the item above stored under every key in `keys`.
async fn seeded(keys: &[&str]) -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for key in keys {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": stored(key)}),
        )
        .await;
    }
    router
}

/// The clause a syntax error states, over the member it was found in.
fn syntax_clause(member: &str, token: &str, near: &str) -> String {
    format!("Invalid {member}: Syntax error; token: \"{token}\", near: \"{near}\"")
}

/// The same clause inside the envelope the members a WRITE states its intent through
/// carry.
fn enveloped(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

#[tokio::test]
async fn a_group_states_the_operand_a_comparison_reads_at_that_position() {
    let router = seeded(&["one"]).await;

    // Each row: the case, the filter, the values it binds and how many items it selects.
    // The rows differ along what the rule generalises over — WHICH operand of the
    // comparison the group wraps (the left one, the right one, a bound of a range, a
    // member of a membership list), WHAT it wraps (a path, a bound value, a `size` call
    // over each type that has a length), and whether the comparison the group feeds holds
    // for the stored item.
    let selected: Vec<(&str, &str, Value, i64)> = vec![
        (
            "a size over a string",
            "(size(s)) = :four",
            json!({":four": {"N": "4"}}),
            1,
        ),
        (
            "a size over a binary value",
            "(size(b)) = :three",
            json!({":three": {"N": "3"}}),
            1,
        ),
        (
            "a size over a string set",
            "(size(ss)) = :two",
            json!({":two": {"N": "2"}}),
            1,
        ),
        (
            "a size over a list",
            "(size(l)) = :two",
            json!({":two": {"N": "2"}}),
            1,
        ),
        (
            "a size over a map",
            "(size(m)) = :one",
            json!({":one": {"N": "1"}}),
            1,
        ),
        (
            "a path as the left operand",
            "(s) = :beta",
            json!({":beta": {"S": "beta"}}),
            1,
        ),
        (
            "a path as the right operand",
            ":beta = (s)",
            json!({":beta": {"S": "beta"}}),
            1,
        ),
        (
            "a value as the right operand",
            "s = (:beta)",
            json!({":beta": {"S": "beta"}}),
            1,
        ),
        (
            "both bounds of a range",
            "size(s) BETWEEN (:one) AND (:nine)",
            json!({":one": {"N": "1"}, ":nine": {"N": "9"}}),
            1,
        ),
        (
            "the left operand of a range",
            "(size(s)) BETWEEN :one AND :nine",
            json!({":one": {"N": "1"}, ":nine": {"N": "9"}}),
            1,
        ),
        (
            "a member of a membership list",
            "size(s) IN ((:four), :nine)",
            json!({":four": {"N": "4"}, ":nine": {"N": "9"}}),
            1,
        ),
        (
            "a grouped operand inside a conjunction",
            "attribute_exists(n) AND (size(s)) = :four",
            json!({":four": {"N": "4"}}),
            1,
        ),
        (
            // The same group, over a comparison the item does not satisfy: the group is
            // READ rather than accepted and ignored.
            "a grouped operand of a comparison that does not hold",
            "(size(s)) = :nine",
            json!({":nine": {"N": "9"}}),
            0,
        ),
        (
            "a grouped ordering comparison the item does not satisfy",
            "(size(s)) > :nine",
            json!({":nine": {"N": "9"}}),
            0,
        ),
        (
            // A group the grammar still reads as a CONDITION, because no comparator
            // follows it.
            "a grouped condition",
            "(s = :beta OR size(l) = :nine)",
            json!({":beta": {"S": "beta"}, ":nine": {"N": "9"}}),
            1,
        ),
        (
            "a grouped condition a negation reads",
            "NOT (size(s) = :nine)",
            json!({":nine": {"N": "9"}}),
            1,
        ),
    ];
    for (case, filter, values, count) in selected {
        let mut request = json!({"TableName": TABLE, "FilterExpression": filter});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let (status, answered) = call(&router, "Scan", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?} ({filter}): expected a served scan, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            answered["Count"],
            json!(count),
            "case {case:?} ({filter}): expected {count} item(s) selected, got {answered}"
        );
    }

    // The same rule in the member a WRITE states its condition through: the condition
    // holds for the stored item, so the write is served.
    let written = ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": stored("one"),
               "ConditionExpression": "(size(s)) = :four",
               "ExpressionAttributeValues": {":four": {"N": "4"}},
               "ReturnValues": "ALL_OLD"}),
    )
    .await;
    assert_eq!(
        written["Attributes"]["s"],
        json!({"S": "beta"}),
        "case a grouped operand in a write's condition: expected the stored item replaced, got \
         {written}"
    );
}

#[tokio::test]
async fn a_group_states_a_whole_set_value_so_a_second_operator_is_a_syntax_error() {
    let router = seeded(&["served", "refused"]).await;

    // Each row: the case, the update, and what the number it writes must read afterwards.
    // The rows differ along the operation the group states (a sum, a difference, a single
    // operand), where the group stands (the whole value, a value nested in another group)
    // and which operand reads the item.
    let served: Vec<(&str, &str, Value, Value)> = vec![
        (
            "a grouped sum",
            "SET tally = (n + :one)",
            json!({":one": {"N": "1"}}),
            json!({"N": "8"}),
        ),
        (
            "a grouped difference",
            "SET tally = (n - :one)",
            json!({":one": {"N": "1"}}),
            json!({"N": "6"}),
        ),
        (
            "a grouped single operand",
            "SET tally = (:one)",
            json!({":one": {"N": "1"}}),
            json!({"N": "1"}),
        ),
        (
            "a grouped path",
            "SET tally = (n)",
            json!({}),
            json!({"N": "7"}),
        ),
    ];
    for (case, expression, values, tally) in served {
        let mut request = json!({"TableName": TABLE, "Key": {"pk": {"S": "served"}},
                                 "UpdateExpression": expression, "ReturnValues": "ALL_NEW"});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let (status, answered) = call(&router, "UpdateItem", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?} ({expression}): expected a served update, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            answered["Attributes"]["tally"], tally,
            "case {case:?} ({expression}): expected the computed number {tally}, got {answered}"
        );
    }

    // A SECOND arithmetic operator states a chain no update computes, and the refusal
    // names the operator where that chain breaks — not the parenthesis the clause opened,
    // and not the operator the value already stated.
    let refused: Vec<(&str, &str, Value, String)> = vec![
        (
            "a difference of a grouped sum",
            "SET tally = (n + :one) - :one",
            json!({":one": {"N": "1"}}),
            syntax_clause("UpdateExpression", "-", ") - :one"),
        ),
        (
            "a sum of a grouped difference",
            "SET tally = (n - :one) + :two",
            json!({":one": {"N": "1"}, ":two": {"N": "2"}}),
            syntax_clause("UpdateExpression", "+", ") + :two"),
        ),
        (
            "a chain through a list index",
            "SET l[0] = (n + :one) - :two",
            json!({":one": {"N": "1"}, ":two": {"N": "2"}}),
            syntax_clause("UpdateExpression", "-", ") - :two"),
        ),
        (
            "the same chain without parentheses",
            "SET tally = n + :one + :two",
            json!({":one": {"N": "1"}, ":two": {"N": "2"}}),
            syntax_clause("UpdateExpression", "+", ":one + :two"),
        ),
    ];
    for (case, expression, values, clause) in refused {
        let request = json!({"TableName": TABLE, "Key": {"pk": {"S": "refused"}},
                             "UpdateExpression": expression,
                             "ExpressionAttributeValues": values});
        let stated = refusal(&router, case, "UpdateItem", request).await;
        let expected = enveloped(&clause);
        assert_eq!(
            stated, expected,
            "case {case:?} ({expression}): expected {expected:?}, got {stated:?}"
        );
    }

    // Nothing of the refused chain was applied: the item the rows above addressed still
    // reads as it was stored.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "refused"}}}),
    )
    .await;
    assert_eq!(
        (
            read["Item"]["n"].clone(),
            read["Item"]["l"].clone(),
            read["Item"].get("tally").cloned()
        ),
        (
            json!({"N": "7"}),
            json!({"L": [{"S": "x"}, {"S": "y"}]}),
            None
        ),
        "case a refused chain: expected the stored item untouched, got {read}"
    );
}

#[tokio::test]
async fn a_list_index_the_lexer_cannot_read_is_refused_at_the_character_that_breaks_it() {
    let router = seeded(&["one"]).await;

    // Each row: the case, the operation, the request, and the whole message it must earn.
    // The rows differ along the expression MEMBER the index arrived in, the operation that
    // carried it, the character that breaks the index (a sign, a letter, the closing
    // bracket, the end of the text), how many digits follow it and whether the name is
    // written out or aliased.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a negative index in a keyed read's projection",
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "#l[-1]", "ExpressionAttributeNames": {"#l": "l"}}),
            syntax_clause("ProjectionExpression", "-", "[-1"),
        ),
        (
            "the same projection on a table walk",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "l[-1]"}),
            syntax_clause("ProjectionExpression", "-", "[-1"),
        ),
        (
            "a negative index of more than one digit",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "l[-12]"}),
            syntax_clause("ProjectionExpression", "-", "[-1"),
        ),
        (
            "a negative index in a later path of the list",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "s, l[-1]"}),
            syntax_clause("ProjectionExpression", "-", "[-1"),
        ),
        (
            "an index with no digits at all",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "l[]"}),
            syntax_clause("ProjectionExpression", "]", "[]"),
        ),
        (
            "an index spelled with a letter",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "l[x]"}),
            syntax_clause("ProjectionExpression", "x", "[x]"),
        ),
        (
            "an index the text never closes",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "l[1"}),
            syntax_clause("ProjectionExpression", "<EOF>", "1"),
        ),
        (
            "a negative index in a filter's operand",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "l[-1] = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            syntax_clause("FilterExpression", "-", "[-1"),
        ),
        (
            "a negative index in a query's filter",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "one"}},
                   "FilterExpression": "attribute_exists(l[-1])"}),
            syntax_clause("FilterExpression", "-", "[-1"),
        ),
        (
            "a negative index in the path an update removes",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "REMOVE l[-1]"}),
            enveloped(&syntax_clause("UpdateExpression", "-", "[-1")),
        ),
        (
            "a negative index in the condition a write states",
            "DeleteItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ConditionExpression": "attribute_exists(l[-1])"}),
            enveloped(&syntax_clause("ConditionExpression", "-", "[-1")),
        ),
    ];
    for (case, operation, request, expected) in rows {
        let stated = refusal(&router, case, operation, request).await;
        assert_eq!(
            stated, expected,
            "case {case:?} ({operation}): expected {expected:?}, got {stated:?}"
        );
    }

    // The index the same paths spell with digits is an ordinary document path, so the
    // rows above are the malformed index being refused rather than the bracket.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "ProjectionExpression": "l[1]"}),
    )
    .await;
    assert_eq!(
        read["Item"], json!({"l": {"L": [{"S": "y"}]}}),
        "case a projection of one list index: expected the addressed element, got {read}"
    );
}

#[tokio::test]
async fn a_legacy_in_entry_is_compared_however_many_values_it_lists() {
    let router = seeded(&["one"]).await;

    /// The values a list of `count` operands holds, the last of them one the item carries.
    fn operands(count: usize) -> Value {
        let mut values: Vec<Value> = (0..count - 1)
            .map(|at| json!({"S": format!("v{at}")}))
            .collect();
        values.push(json!({"S": "beta"}));
        Value::Array(values)
    }

    // The rows span the operand counts the rule generalises over: one value, the count the
    // EXPRESSION operator is held to, one past it, and well past it. Every one of them is
    // compared against the item the scan reads.
    for count in [1usize, 100, 101, 300] {
        let (status, answered) = call(
            &router,
            "Scan",
            json!({"TableName": TABLE,
                   "ScanFilter": {"s": {"ComparisonOperator": "IN",
                                        "AttributeValueList": operands(count)}}}),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case a legacy IN listing {count} value(s): expected a served scan, got {status} \
             with {}",
            message(&answered)
        );
        assert_eq!(
            answered["Count"],
            json!(1),
            "case a legacy IN listing {count} value(s): expected the item holding a listed \
             value, got {answered}"
        );
    }

    // A list of any length answers the same question, so the rows below span what the
    // comparison compares: the attribute the entry is over, the operand TYPE the list
    // holds, whether the stored value is in it, and whether the stored value is even of a
    // type the list could hold. A number is compared by the NUMBER it denotes, so a
    // listed `7.0` matches a stored `7`.
    let comparisons: Vec<(&str, &str, Value, i64)> = vec![
        ("a string the item holds", "s", json!([{"S": "beta"}]), 1),
        ("a string the item does not", "s", json!([{"S": "gamma"}]), 0),
        (
            "a number spelled another way",
            "n",
            json!([{"N": "1"}, {"N": "7.0"}]),
            1,
        ),
        ("a number the item does not hold", "n", json!([{"N": "8"}]), 0),
        ("the binary value the item holds", "b", json!([{"B": "AQID"}]), 1),
        (
            "a binary value the item does not",
            "b",
            json!([{"B": "AQIE"}]),
            0,
        ),
        (
            // The stored value is a SET and the operands are strings, which no `IN`
            // operand list can hold: a member of that set is not the set.
            "a member of the set attribute the item holds",
            "ss",
            json!([{"S": "a"}, {"S": "b"}]),
            0,
        ),
        (
            "a value for an attribute the item does not hold at all",
            "absent",
            json!([{"S": "beta"}]),
            0,
        ),
    ];
    for (case, attribute, list, count) in comparisons {
        let (status, answered) = call(
            &router,
            "Scan",
            json!({"TableName": TABLE,
                   "ScanFilter": {attribute: {"ComparisonOperator": "IN",
                                              "AttributeValueList": list}}}),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case a legacy IN over {case:?}: expected a served scan, got {status} with {}",
            message(&answered)
        );
        assert_eq!(
            answered["Count"],
            json!(count),
            "case a legacy IN over {case:?}: expected {count} item(s) selected, got {answered}"
        );
    }

    // The ceiling the expression's own `IN` operator carries is unchanged: the same list
    // written as operands of that operator is refused past 100 of them.
    let placeholders: Vec<String> = (0..101).map(|at| format!(":v{at}")).collect();
    let values: serde_json::Map<String, Value> = placeholders
        .iter()
        .map(|placeholder| (placeholder.clone(), json!({"S": "beta"})))
        .collect();
    let stated = refusal(
        &router,
        "an expression IN past the documented ceiling",
        "Scan",
        json!({"TableName": TABLE,
               "FilterExpression": format!("s IN ({})", placeholders.join(", ")),
               "ExpressionAttributeValues": Value::Object(values)}),
    )
    .await;
    assert_eq!(
        stated,
        "Invalid FilterExpression: The IN operator is provided with too many operands; number \
         of operands: 101",
        "case an expression IN past the documented ceiling: expected the operand-count \
         sentence, got {stated:?}"
    );
}

#[tokio::test]
async fn an_update_through_a_path_that_addresses_no_location_is_refused() {
    let router = seeded(&["one"]).await;

    // Each row: the case, the update and the values it binds. The rows differ along the
    // ACTION stating the path (`REMOVE`, `ADD`, `DELETE`, `SET`), which step of the path
    // reaches nothing (the first, a later one), and why it reaches nothing (the attribute
    // is not there, the map has no such key, the value is a scalar, the step is an index
    // into something that is not a list, the index is past the end of a list).
    let refused: Vec<(&str, &str, Value)> = vec![
        (
            "a REMOVE through an attribute the item does not hold",
            "REMOVE absent.leaf",
            json!({}),
        ),
        (
            "a REMOVE through a key the map does not hold",
            "REMOVE m.absent.leaf",
            json!({}),
        ),
        ("a REMOVE through a scalar", "REMOVE s.leaf", json!({})),
        ("a REMOVE indexing a map", "REMOVE m[0]", json!({})),
        (
            "a REMOVE through an index past the end of a list",
            "REMOVE l[5].leaf",
            json!({}),
        ),
        (
            "a DELETE through an attribute the item does not hold",
            "DELETE absent.leaf :members",
            json!({":members": {"SS": ["a"]}}),
        ),
        (
            "a DELETE through a scalar",
            "DELETE s.leaf :members",
            json!({":members": {"SS": ["a"]}}),
        ),
        (
            "an ADD through an attribute the item does not hold",
            "ADD absent.leaf :one",
            json!({":one": {"N": "1"}}),
        ),
        (
            "an ADD indexing a map",
            "ADD m[0] :one",
            json!({":one": {"N": "1"}}),
        ),
        (
            "a SET through an attribute the item does not hold",
            "SET absent.leaf = :one",
            json!({":one": {"N": "1"}}),
        ),
    ];
    for (case, expression, values) in refused {
        let mut request = json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                                 "UpdateExpression": expression});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let stated = refusal(&router, case, "UpdateItem", request).await;
        assert_eq!(
            stated, INVALID_PATH,
            "case {case:?} ({expression}): expected the invalid-path sentence, got {stated:?}"
        );
    }

    // The paths that DO address a location of the item are served, whether or not that
    // location holds anything: the rows above are refused for the step that reaches
    // nothing, not for the value the action would have found there.
    let served: Vec<(&str, &str, Value)> = vec![
        (
            "a REMOVE of a top-level attribute the item does not hold",
            "REMOVE absent",
            json!({}),
        ),
        (
            "a REMOVE of an index past the end of a list",
            "REMOVE l[5]",
            json!({}),
        ),
        (
            "a DELETE from an attribute the item does not hold",
            "DELETE absent :members",
            json!({":members": {"SS": ["a"]}}),
        ),
        (
            "a SET creating an entry of a map the item holds",
            "SET m.fresh = :one",
            json!({":one": {"N": "1"}}),
        ),
    ];
    for (case, expression, values) in served {
        let mut request = json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                                 "UpdateExpression": expression});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let (status, answered) = call(&router, "UpdateItem", request).await;
        assert_eq!(
            status,
            200,
            "case {case:?} ({expression}): expected a served update, got {status} with {}",
            message(&answered)
        );
    }

    // The one row that wrote anything wrote it where it said: the map the item holds
    // carries the new entry, and the attributes the refused rows named are still absent.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}}}),
    )
    .await;
    assert_eq!(
        read["Item"]["m"],
        json!({"M": {"leaf": {"S": "x"}, "fresh": {"N": "1"}}}),
        "case a SET creating a map entry: expected the entry written beside the stored one, got \
         {read}"
    );
    assert_eq!(
        read["Item"]["s"],
        json!({"S": "beta"}),
        "case the refused rows: expected the stored scalar untouched, got {read}"
    );
}

#[tokio::test]
async fn an_update_operand_type_refusal_follows_where_the_type_came_from() {
    let router = seeded(&["one"]).await;

    // Each row: the case, the update, the values it binds and the message it must earn.
    // The rows differ along the operator or function that could not take the operand
    // (`+`, `-`, `list_append`), which position it arrived in, the type it carried, and
    // WHERE that type came from — the request's own text, or the stored item. The second
    // is the only thing the two halves differ in.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a sum over a type the request spelled",
            "SET tally = :word + :one",
            json!({":word": {"S": "X"}, ":one": {"N": "1"}}),
            enveloped(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: +, operand type: S",
            ),
        ),
        (
            "a difference over a type the request spelled",
            "SET tally = :one - :word",
            json!({":word": {"S": "X"}, ":one": {"N": "1"}}),
            enveloped(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: -, operand type: S",
            ),
        ),
        (
            "a list_append whose second operand the request spelled",
            "SET l = list_append(l, :word)",
            json!({":word": {"S": "X"}}),
            enveloped(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: list_append, operand type: S",
            ),
        ),
        (
            "a list_append whose first operand the request spelled",
            "SET l = list_append(:number, l)",
            json!({":number": {"N": "1"}}),
            enveloped(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: list_append, operand type: N",
            ),
        ),
        (
            "a fallback the request spelled, taken because the item holds nothing there",
            "SET tally = if_not_exists(absent, :word) + :one",
            json!({":word": {"S": "X"}, ":one": {"N": "1"}}),
            enveloped(
                "Invalid UpdateExpression: Incorrect operand type for operator or function; \
                 operator or function: +, operand type: S",
            ),
        ),
        (
            "a sum over a type the item holds",
            "SET tally = s + :one",
            json!({":one": {"N": "1"}}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
        (
            "a difference over a type the item holds",
            "SET tally = n - s",
            json!({}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
        (
            "a sum over a map the item holds",
            "SET tally = m + :one",
            json!({":one": {"N": "1"}}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
        (
            "a list_append whose second operand the item holds",
            "SET tally = list_append(l, s)",
            json!({}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
        (
            "a list_append whose first operand the item holds",
            "SET tally = list_append(m, l)",
            json!({}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
        (
            "a fallback the item holds, taken because the attribute is there",
            "SET tally = if_not_exists(s, :one) + :one",
            json!({":one": {"N": "1"}}),
            STORED_TYPE_MISMATCH.to_string(),
        ),
    ];
    for (case, expression, values, expected) in rows {
        let mut request = json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                                 "UpdateExpression": expression});
        if !values.as_object().is_none_or(serde_json::Map::is_empty) {
            request["ExpressionAttributeValues"] = values;
        }
        let stated = refusal(&router, case, "UpdateItem", request).await;
        assert_eq!(
            stated, expected,
            "case {case:?} ({expression}): expected {expected:?}, got {stated:?}"
        );
    }
}
