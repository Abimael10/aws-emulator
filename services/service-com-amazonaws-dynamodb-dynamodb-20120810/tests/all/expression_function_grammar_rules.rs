// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a FUNCTION CALL earns from the grammar it was written in, told apart from
//! the refusal a name the expression languages do not have earns.
//!
//! RULE: each expression grammar defines its own functions — the `ConditionExpression`
//! reference lists `attribute_exists`, `attribute_not_exists`, `attribute_type`,
//! `begins_with`, `contains` and `size`, and the `UpdateItem$UpdateExpression` reference
//! lists `if_not_exists` and `list_append` — and a call is refused by WHICH of three facts
//! it breaks:
//!
//! 1. a call to a function the grammar reading the text does not define, where the
//!    expression languages define it elsewhere, is refused for the CONTEXT: "The function
//!    is not allowed in a condition expression; function: <name>" and "… in an update
//!    expression; function: <name>";
//! 2. a call to a name NONE of them defines keeps the unknown-name sentence, "Invalid
//!    function name; function: <name>" — which is the control that tells the two defect
//!    classes apart;
//! 3. a call the grammar DOES define, written where the grammar states an operand rather
//!    than what that function states, is refused for the POSITION: "The function is not
//!    allowed to be used this way in an expression; function: <name>". A function that
//!    states a CONDITION (`attribute_exists` and the four beside it) states no value, so
//!    it fills no operand position; `size` states a value, so it fills no position where a
//!    condition belongs.
//!
//! Each clause is stated behind the member that carried the text, inside the
//! `<N> validation error(s) detected: ` envelope exactly where the members a WRITE states
//! its intent through carry it.
//!
//! The rows differ along what the rule generalises over: the FUNCTION at fault, the GRAMMAR
//! and member that carried it (`ConditionExpression`, `FilterExpression`,
//! `UpdateExpression`), the POSITION the call was written in (a whole condition, either
//! operand of a comparison, a range bound, a membership list, a path argument, a value
//! argument) and the OPERATION that read it — and the served controls at the end show the
//! refusals are the rule and not a blanket refusal of a call.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::probe::refusal;
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "FunctionGrammarRules";

/// The key of the one seeded item.
fn key() -> Value {
    json!({"pk": {"S": "item-1"}})
}

/// The emulator with the table and one item in place, so no row is refused for its table or
/// its key instead of for the rule under test.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "item-1"},
            "s": {"S": "beta"},
            "n": {"N": "1"},
            "l": {"L": [{"S": "one"}]},
        }}),
    )
    .await;
    router
}

/// The clause a refusal about `member` states.
fn clause(member: &str, detail: &str) -> String {
    format!("Invalid {member}: {detail}")
}

/// The same clause inside the envelope the write-intent members state their refusals in.
fn enveloped(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The whole message a call to a function the grammar does not define earns.
fn not_in_this_grammar(member: &str, grammar: &str, function: &str) -> String {
    clause(
        member,
        &format!("The function is not allowed in {grammar}; function: {function}"),
    )
}

/// The whole message a call written where the grammar states an operand earns.
fn not_this_way(member: &str, function: &str) -> String {
    clause(
        member,
        &format!(
            "The function is not allowed to be used this way in an expression; function: \
             {function}"
        ),
    )
}

/// The whole message a name no grammar of this service defines earns.
fn unknown(member: &str, function: &str) -> String {
    clause(
        member,
        &format!("Invalid function name; function: {function}"),
    )
}

/// How a refusal names each grammar.
const CONDITION: &str = "a condition expression";
const UPDATE: &str = "an update expression";

/// One row: the case, the operation, the request, and the whole message it must earn.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected: String,
}

/// The rows for rule 1 and its control, rule 2: a function of the OTHER grammar, and a name
/// of neither.
fn grammar_rows() -> Vec<Row> {
    vec![
        Row {
            // `if_not_exists` is an UPDATE function, so a condition may not call it.
            case: "if_not_exists in an UpdateItem condition",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :v",
                            "ConditionExpression": "if_not_exists(s, :v)",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: enveloped(&not_in_this_grammar(
                "ConditionExpression",
                CONDITION,
                "if_not_exists",
            )),
        },
        Row {
            case: "list_append in a PutItem condition",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "list_append(l, :v) = :v",
                            "ExpressionAttributeValues": {":v": {"L": []}}}),
            expected: enveloped(&not_in_this_grammar(
                "ConditionExpression",
                CONDITION,
                "list_append",
            )),
        },
        Row {
            // The same rule behind the member a READ narrows its result with, which states
            // its clause without the envelope.
            case: "if_not_exists in a Scan filter",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "if_not_exists(s, :v) = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: not_in_this_grammar("FilterExpression", CONDITION, "if_not_exists"),
        },
        Row {
            // And the mirror image: a CONDITION function written in an update.
            case: "contains in an UpdateExpression",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = contains(s, :v)",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: enveloped(&not_in_this_grammar(
                "UpdateExpression",
                UPDATE,
                "contains",
            )),
        },
        Row {
            case: "attribute_exists in an UpdateExpression",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = attribute_exists(s)"}),
            expected: enveloped(&not_in_this_grammar(
                "UpdateExpression",
                UPDATE,
                "attribute_exists",
            )),
        },
        Row {
            // `size` states a value, and the update grammar still defines no call to it.
            case: "size in an UpdateExpression",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET n = size(s)"}),
            expected: enveloped(&not_in_this_grammar("UpdateExpression", UPDATE, "size")),
        },
        Row {
            // The same expression inside a TRANSACTION is read by the same parser, and an
            // element's own members state the clause without the envelope.
            case: "contains in a transaction element's update",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [{"Update": {
                "TableName": TABLE, "Key": key(),
                "UpdateExpression": "SET s = contains(s, :v)",
                "ExpressionAttributeValues": {":v": {"S": "x"}},
            }}]}),
            expected: not_in_this_grammar("UpdateExpression", UPDATE, "contains"),
        },
        // Rule 2, the control: a name NEITHER grammar defines keeps the unknown-name
        // sentence, so the clauses above report a context and not a spelling.
        Row {
            case: "a misspelled attribute_exists in a Scan filter",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_exist(s)"}),
            expected: unknown("FilterExpression", "attribute_exist"),
        },
        Row {
            case: "a misspelled list_append in an UpdateExpression",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET l = list_appnd(l, :v)",
                            "ExpressionAttributeValues": {":v": {"L": []}}}),
            expected: enveloped(&unknown("UpdateExpression", "list_appnd")),
        },
    ]
}

/// The rows for rule 3: a call the grammar defines, written where it states an operand (or,
/// for the one function whose value IS an operand, where it states a condition).
fn position_rows() -> Vec<Row> {
    vec![
        Row {
            // A condition function states no value, so a comparator written after its call
            // reads that condition as an operand.
            case: "attribute_exists as the left side of a range",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_exists(s) BETWEEN :a AND :b",
                            "ExpressionAttributeValues": {":a": {"S": "a"},
                                                          ":b": {"S": "z"}}}),
            expected: not_this_way("FilterExpression", "attribute_exists"),
        },
        Row {
            case: "attribute_not_exists as the left side of an equality",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_not_exists(s) = :a",
                            "ExpressionAttributeValues": {":a": {"S": "a"}}}),
            expected: not_this_way("FilterExpression", "attribute_not_exists"),
        },
        Row {
            case: "contains as the left side of a membership test",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :k",
                            "FilterExpression": "contains(s, :a) IN (:a)",
                            "ExpressionAttributeValues": {":k": {"S": "item-1"},
                                                          ":a": {"S": "a"}}}),
            expected: not_this_way("FilterExpression", "contains"),
        },
        Row {
            // The operand the expression wrote FIRST is read the same way.
            case: "begins_with as the right side of an equality",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": ":a = begins_with(s, :a)",
                            "ExpressionAttributeValues": {":a": {"S": "a"}}}),
            expected: not_this_way("FilterExpression", "begins_with"),
        },
        Row {
            // A call written in the PATH argument of another call: the inner function
            // states a condition, so the refusal names IT rather than the path the
            // enclosing position needs.
            case: "contains inside a size call",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :v",
                            "ConditionExpression": "size(contains(s, :v)) = :n",
                            "ExpressionAttributeValues": {":v": {"S": "x"},
                                                          ":n": {"N": "1"}}}),
            expected: enveloped(&not_this_way("ConditionExpression", "contains")),
        },
        Row {
            case: "attribute_type inside an attribute_exists call",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_exists(attribute_type(s, :t))",
                            "ExpressionAttributeValues": {":t": {"S": "S"}}}),
            expected: not_this_way("FilterExpression", "attribute_type"),
        },
        Row {
            // A call written in a VALUE argument of another call.
            case: "attribute_exists inside a begins_with call",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "begins_with(s, attribute_exists(s))"}),
            expected: not_this_way("FilterExpression", "attribute_exists"),
        },
        Row {
            // `size` is the function whose value IS an operand, so the position it cannot
            // stand in is the one a CONDITION belongs in: a bare call states no condition.
            case: "a bare size call as the whole filter",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "size(s)"}),
            expected: not_this_way("FilterExpression", "size"),
        },
        Row {
            case: "a bare size call as one side of a conjunction",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :v",
                            "ConditionExpression": "attribute_exists(s) AND size(s)",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: enveloped(&not_this_way("ConditionExpression", "size")),
        },
    ]
}

#[tokio::test]
async fn a_call_outside_its_grammar_is_told_apart_from_an_unknown_name() {
    let router = seeded().await;

    for Row {
        case,
        operation,
        request,
        expected,
    } in grammar_rows()
    {
        let stated = refusal(&router, case, operation, request.clone()).await;
        assert_eq!(
            stated, expected,
            "case {case:?} ({operation} {request}): expected {expected:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn a_call_written_where_the_grammar_states_something_else_is_refused_for_its_position() {
    let router = seeded().await;

    for Row {
        case,
        operation,
        request,
        expected,
    } in position_rows()
    {
        let stated = refusal(&router, case, operation, request.clone()).await;
        assert_eq!(
            stated, expected,
            "case {case:?} ({operation} {request}): expected {expected:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn every_call_each_grammar_defines_in_its_own_position_is_still_served() {
    let router = seeded().await;

    // The control for both rules: each function in the position its own grammar states it
    // in is READ and answered, so the refusals above are about the grammar and the position
    // and not about calls in expressions.
    for (case, filter, values) in [
        ("attribute_exists", "attribute_exists(s)", json!({})),
        (
            "attribute_not_exists",
            "attribute_not_exists(nowhere)",
            json!({}),
        ),
        (
            "attribute_type",
            "attribute_type(s, :t)",
            json!({":t": {"S": "S"}}),
        ),
        (
            "begins_with",
            "begins_with(s, :p)",
            json!({":p": {"S": "be"}}),
        ),
        ("contains", "contains(s, :p)", json!({":p": {"S": "et"}})),
        ("size as an operand", "size(s) = :n", json!({":n": {"N": "4"}})),
    ] {
        let mut request = json!({"TableName": TABLE, "ConsistentRead": true,
                                 "FilterExpression": filter});
        if values != json!({}) {
            request["ExpressionAttributeValues"] = values;
        }
        let page = ok(&router, "Scan", request).await;
        assert_eq!(
            page["Count"],
            json!(1),
            "case {case:?}: the call is served over the stored item, got {page}"
        );
    }

    // And the two the UPDATE grammar defines, in the nesting its own reference writes.
    let updated = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression":
                   "SET l = list_append(if_not_exists(l, :empty), :more), s = if_not_exists(s, :v)",
               "ExpressionAttributeValues": {":empty": {"L": []},
                                             ":more": {"L": [{"S": "two"}]},
                                             ":v": {"S": "other"}},
               "ReturnValues": "UPDATED_NEW"}),
    )
    .await;
    assert_eq!(
        (
            updated["Attributes"]["l"].clone(),
            updated["Attributes"]["s"].clone()
        ),
        (
            json!({"L": [{"S": "one"}, {"S": "two"}]}),
            json!({"S": "beta"})
        ),
        "case the update grammar's own calls: expected the list appended and the stored \
         string kept, got {updated}"
    );

    // Read back through the implemented reader, so what the served update left is the
    // stored item and not the response alone.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key(), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["l"],
        json!({"L": [{"S": "one"}, {"S": "two"}]}),
        "case the served nesting: expected the appended list stored, got {read}"
    );
}
