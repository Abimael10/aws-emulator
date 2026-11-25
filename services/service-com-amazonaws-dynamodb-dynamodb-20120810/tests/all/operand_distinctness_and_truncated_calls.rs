// SPDX-License-Identifier: CC0-1.0

//! WHICH defect an expression that relates a location to itself, or that runs out of
//! text, is refused for.
//!
//! Two rules of the condition-expression language are pinned here, each over the whole
//! class it covers rather than over one recorded call:
//!
//! 1. An operator or function whose FIRST operand names the same document path as one of
//!    its remaining operands is refused for NON-DISTINCTNESS — "The first operand must be
//!    distinct from the remaining operands for this operator or function; operator:
//!    <operator>, first operand: [<path>]" — naming the operator the expression wrote and
//!    the path it repeated, and never for the KIND of operand the position holds: a
//!    document path is a legal operand of `contains`, `begins_with` and every comparator,
//!    so the operand-type sentence would point a caller at a signature that is not what is
//!    wrong ("contains (path, operand) ... The path and the operand must be distinct; for
//!    example, contains (a, a) returns an error" — the `ConditionExpression` function
//!    reference, stated of "this operator or function" generally).
//!
//! 2. An expression whose text ENDS where the grammar still needs a token — inside a call,
//!    after a separator, after an operand, before a bound, before a closing parenthesis —
//!    is a SYNTAX error reported at the position the text ran out, `Syntax error; token:
//!    "<EOF>", near: "<the expression's own text there>"`, ahead of every rule about the
//!    call it was writing: how many operands that call carries, and what kind each of them
//!    is, are facts of a call the expression FINISHED, so a call the parse could not
//!    complete is not a call of the wrong arity or with the wrong operand kind — the
//!    refusal points at the position rather than at the function's signature.
//!
//! The rows differ along what each rule generalises over: the expression MEMBER the text
//! arrived in (`ConditionExpression`, `FilterExpression`, `UpdateExpression`), the
//! operation carrying it (all three single-item writes, both table walks, a transaction
//! element), whether the member
//! is the request's own or one element's, the operator or function at fault (`=`, `<`,
//! `BETWEEN`, `IN`, `contains`, `begins_with`, `attribute_exists`,
//! `attribute_not_exists`, `attribute_type`, `size`, `if_not_exists`, `list_append`), the
//! FORM of the repeated path (a bare name, an `#alias`, two different aliases for one
//! name, a nested path, a list index) and WHERE the text ran out (after `(`, after a
//! separator, after an operand, after `AND`).
//!
//! The control suite that CLOSES the same calls shows the operand rules still being
//! judged of them, so the rows above are the position being reported and not those rules
//! going unreported.
//!
//! Which refusals carry the `<N> validation error(s) detected: ` envelope is this
//! service's existing split (the members a WRITE states its intent through carry it, the
//! members a READ narrows its result with do not, and an ELEMENT's members never do), so
//! each row states the whole message it must earn.
//!
//! Every call is driven over the wire, and the closing suites show the same shapes with
//! DISTINCT operands and with the text completed being served — so the rows above are
//! these two classifications and not a blanket rejection.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: a string partition key, so a write row has a key to
/// state and a read row a table to walk.
const TABLE: &str = "OperandDistinctness";

/// The key of the one seeded item, which the write rows address.
fn key() -> Value {
    json!({"pk": {"S": "item-1"}})
}

/// The emulator with the table and one item in place, so no row is refused for its table
/// or its key instead of for the rule under test.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "item-1"},
                        "tagset": {"SS": ["a", "b"]},
                        "score": {"N": "5"},
                        "s": {"S": "beta"},
                        "peer": {"S": "beta"},
                        "l": {"L": [{"S": "x"}]},
                        "m": {"M": {"leaf": {"S": "x"}}}}}),
    )
    .await;
    router
}

/// The clause a refusal about `member` states, as that member's refusals are worded.
fn clause(member: &str, detail: &str) -> String {
    format!("Invalid {member}: {detail}")
}

/// The same clause inside the envelope the write-intent members state their refusals in.
fn enveloped(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The whole message an operator or function relating a location to itself earns, over the
/// member that carried it: the operator as the expression wrote it, and the repeated path
/// as the token list AWS's refusals print a document path as.
fn non_distinct(member: &str, operator: &str, first_operand: &str) -> String {
    clause(
        member,
        &format!(
            "The first operand must be distinct from the remaining operands for this operator \
             or function; operator: {operator}, first operand: {first_operand}"
        ),
    )
}

/// The whole message an expression that ran out of text earns, over the member that
/// carried it.
fn ran_out(member: &str, near: &str) -> String {
    clause(
        member,
        &format!("Syntax error; token: \"<EOF>\", near: \"{near}\""),
    )
}

/// The sentence a call carrying the wrong NUMBER of operands earns — asserted by the
/// control suite below, so the truncated rows are the position being reported rather than
/// the operand rules never being reported at all.
fn operand_count(function: &str, operands: usize) -> String {
    format!(
        "Incorrect number of operands for operator or function; operator or function: \
         {function}, number of operands: {operands}"
    )
}

/// One row: the case, the operation, the request, and the whole message it must earn.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected: String,
}

/// Drive every row and hold each to its status, error code and whole message.
async fn each_row_is_refused(router: &MultiServiceRouter, rows: Vec<Row>) {
    for Row {
        case,
        operation,
        request,
        expected,
    } in rows
    {
        let (status, answered) = call(router, operation, request.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({operation} {request}): expected {expected:?}, got {status} with \
             {answered}"
        );
    }
}

#[tokio::test]
async fn an_operator_relating_a_location_to_itself_is_refused_for_non_distinctness() {
    let router = seeded().await;

    let rows = vec![
        Row {
            case: "an update's condition searching a set for itself",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET score = :nv",
                            "ConditionExpression": "contains(tagset, tagset)",
                            "ExpressionAttributeValues": {":nv": {"N": "999"}}}),
            expected: enveloped(&non_distinct("ConditionExpression", "contains", "[tagset]")),
        },
        Row {
            // The same call with the repeated path written behind ONE alias: the refusal
            // names the attribute the alias stands for, which is the location the
            // expression repeated.
            case: "a put's condition searching an aliased set for itself",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "contains(#t, #t)",
                            "ExpressionAttributeNames": {"#t": "tagset"}}),
            expected: enveloped(&non_distinct("ConditionExpression", "contains", "[tagset]")),
        },
        Row {
            // And with TWO aliases for one name: distinctness is a question about the
            // locations the operands address, not about how the text spells them.
            case: "a delete's condition searching a set for itself through two aliases",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "contains(#a, #b)",
                            "ExpressionAttributeNames": {"#a": "tagset", "#b": "tagset"}}),
            expected: enveloped(&non_distinct("ConditionExpression", "contains", "[tagset]")),
        },
        Row {
            case: "a delete's condition testing a string as its own prefix",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "begins_with(s, s)"}),
            expected: enveloped(&non_distinct("ConditionExpression", "begins_with", "[s]")),
        },
        Row {
            case: "a put's condition comparing an attribute with itself",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "score = score"}),
            expected: enveloped(&non_distinct("ConditionExpression", "=", "[score]")),
        },
        Row {
            case: "an update's condition ordering an attribute against itself",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET score = :nv",
                            "ConditionExpression": "score < score",
                            "ExpressionAttributeValues": {":nv": {"N": "999"}}}),
            expected: enveloped(&non_distinct("ConditionExpression", "<", "[score]")),
        },
        Row {
            case: "a put's condition ranging an attribute against itself as a bound",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "score BETWEEN score AND :hi",
                            "ExpressionAttributeValues": {":hi": {"N": "9"}}}),
            expected: enveloped(&non_distinct("ConditionExpression", "BETWEEN", "[score]")),
        },
        Row {
            case: "a put's condition asking whether an attribute is one of itself",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "s IN (:v, s)",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: enveloped(&non_distinct("ConditionExpression", "IN", "[s]")),
        },
        Row {
            // A READ's member states the same clause without the envelope.
            case: "a scan's filter searching a set for itself",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "contains(tagset, tagset)"}),
            expected: non_distinct("FilterExpression", "contains", "[tagset]"),
        },
        Row {
            case: "a query's filter comparing a nested path with itself",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"}},
                            "FilterExpression": "m.leaf = m.leaf"}),
            expected: non_distinct("FilterExpression", "=", "[m, leaf]"),
        },
        Row {
            case: "a scan's filter searching a list element for itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "contains(l[0], l[0])"}),
            expected: non_distinct("FilterExpression", "contains", "[l, [0]]"),
        },
        Row {
            // Nested under NOT and inside a group: the defect is the operator's whatever
            // the expression wraps it in.
            case: "a scan's filter negating a comparison of an attribute with itself",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "NOT (s = s) AND attribute_exists(pk)"}),
            expected: non_distinct("FilterExpression", "=", "[s]"),
        },
        Row {
            // An ELEMENT's own members state the clause on their own, like every other
            // refusal about one element of a transaction.
            case: "a transaction element's condition searching a set for itself",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [
                {"Update": {"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET score = :nv",
                            "ConditionExpression": "contains(tagset, tagset)",
                            "ExpressionAttributeValues": {":nv": {"N": "999"}}}}]}),
            expected: non_distinct("ConditionExpression", "contains", "[tagset]"),
        },
    ];

    each_row_is_refused(&router, rows).await;
}

#[tokio::test]
async fn an_expression_that_runs_out_of_text_is_refused_as_a_syntax_error() {
    let router = seeded().await;

    let rows = vec![
        Row {
            case: "a put's condition ending just inside a one-operand call",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_exists("}),
            expected: enveloped(&ran_out("ConditionExpression", "(")),
        },
        Row {
            case: "an update's condition ending just inside a two-operand call",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET score = :nv",
                            "ConditionExpression": "contains(",
                            "ExpressionAttributeValues": {":nv": {"N": "999"}}}),
            expected: enveloped(&ran_out("ConditionExpression", "(")),
        },
        Row {
            case: "a delete's condition ending just inside attribute_not_exists",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "attribute_not_exists("}),
            expected: enveloped(&ran_out("ConditionExpression", "(")),
        },
        Row {
            case: "a scan's filter ending after a call's separator",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(s,"}),
            expected: ran_out("FilterExpression", ","),
        },
        Row {
            case: "a scan's filter ending after a call's first operand",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_type(s"}),
            expected: ran_out("FilterExpression", "s"),
        },
        Row {
            // The same truncation after a SEPARATOR and a second operand: the operand
            // positions a closed call would be held to say nothing about text that never
            // finished the call.
            case: "a scan's filter ending after a two-operand call's second operand",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_type(s, :t",
                            "ExpressionAttributeValues": {":t": {"S": "S"}}}),
            expected: ran_out("FilterExpression", ":t"),
        },
        Row {
            // A prefix operand of a type `begins_with` cannot take, inside a call the
            // expression never closed: the text ran out, which is what is wrong with it.
            case: "a put's condition ending after a prefix operand of the wrong type",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "begins_with(s, :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: enveloped(&ran_out("ConditionExpression", ":n")),
        },
        Row {
            case: "a scan's filter ending after a size call's path",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "size(tagset"}),
            expected: ran_out("FilterExpression", "tagset"),
        },
        Row {
            case: "a scan's filter ending inside a size call",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "size("}),
            expected: ran_out("FilterExpression", "("),
        },
        Row {
            case: "a query's filter ending before a range's upper bound",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "score BETWEEN :lo AND",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"},
                                                          ":lo": {"N": "1"}}}),
            expected: ran_out("FilterExpression", "AND"),
        },
        Row {
            case: "a scan's filter ending inside a membership list",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "s IN (:v,",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: ran_out("FilterExpression", ","),
        },
        Row {
            case: "an update's SET value ending just inside if_not_exists",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET score = if_not_exists("}),
            expected: enveloped(&ran_out("UpdateExpression", "(")),
        },
        Row {
            case: "an update's SET value ending just inside list_append",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET l = list_append("}),
            expected: enveloped(&ran_out("UpdateExpression", "(")),
        },
        Row {
            case: "an update's SET value ending after list_append's first operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET l = list_append(l"}),
            expected: enveloped(&ran_out("UpdateExpression", "l")),
        },
        Row {
            case: "a transaction element's condition ending just inside a call",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [
                {"Put": {"TableName": TABLE, "Item": key(),
                         "ConditionExpression": "attribute_exists("}}]}),
            expected: ran_out("ConditionExpression", "("),
        },
    ];

    each_row_is_refused(&router, rows).await;
}

#[tokio::test]
async fn a_call_the_expression_closes_is_held_to_its_operands() {
    let router = seeded().await;

    // The arity and operand-kind sentences are for a call whose operand list the
    // expression finished: too many or too few operands, or an operand of a kind the
    // position does not take. So the truncated rows above are the position being reported
    // and not these rules going unreported.
    let rows = vec![
        Row {
            case: "a scan's filter giving attribute_exists two operands",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_exists(s, score)"}),
            expected: clause(
                "FilterExpression",
                &operand_count("attribute_exists", 2),
            ),
        },
        Row {
            case: "a put's condition giving contains three operands",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "contains(tagset, :a, :b)",
                            "ExpressionAttributeValues": {":a": {"S": "a"},
                                                          ":b": {"S": "b"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_count("contains", 3),
            )),
        },
        Row {
            case: "a scan's filter giving begins_with one operand",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(s)"}),
            expected: clause("FilterExpression", &operand_count("begins_with", 1)),
        },
        Row {
            // The same call CLOSED, with the prefix operand of a type the function cannot
            // take: a call the expression finished is held to its operands' kinds.
            case: "a scan's filter closing begins_with over a numeric prefix",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(s, :n)",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: clause(
                "FilterExpression",
                "Incorrect operand type for operator or function; operator or function: \
                 begins_with, operand type: N",
            ),
        },
        Row {
            case: "a scan's filter closing attribute_exists over a value placeholder",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_exists(:v)",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            expected: clause(
                "FilterExpression",
                "Operator or function requires a document path; operator or function: \
                 attribute_exists",
            ),
        },
    ];

    each_row_is_refused(&router, rows).await;
}

#[tokio::test]
async fn the_same_shapes_with_distinct_operands_and_complete_text_are_served() {
    let router = seeded().await;

    // `contains` over two DIFFERENT locations, and over a path and a value, are both
    // evaluated against the stored item: the rows above are the repeated location being
    // refused rather than a document path being rejected as an operand.
    let evaluated = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET score = :nv",
               "ConditionExpression": "contains(tagset, :member) AND NOT contains(tagset, s)",
               "ExpressionAttributeValues": {":nv": {"N": "7"}, ":member": {"S": "a"}},
               "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(
        evaluated["Attributes"]["score"],
        json!({"N": "7"}),
        "case a condition whose operands name two different locations: expected the update \
         applied, got {evaluated}"
    );

    // And the truncated calls above, written out in full, are read as the calls they are.
    let scanned = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE,
               "FilterExpression": "attribute_exists(tagset) AND begins_with(s, :p) AND \
                                    size(tagset) = :n AND attribute_type(s, :t)",
               "ExpressionAttributeValues": {":p": {"S": "be"}, ":n": {"N": "2"},
                                             ":t": {"S": "S"}}}),
    )
    .await;
    assert_eq!(
        scanned["Count"],
        json!(1),
        "case the same calls written out in full: expected the seeded item selected, got \
         {scanned}"
    );

    let appended = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET l = list_append(l, :more), s = if_not_exists(s, :fresh)",
               "ExpressionAttributeValues": {":more": {"L": [{"S": "y"}]},
                                             ":fresh": {"S": "unused"}},
               "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(
        (&appended["Attributes"]["l"], &appended["Attributes"]["s"]),
        (
            &json!({"L": [{"S": "x"}, {"S": "y"}]}),
            &json!({"S": "beta"})
        ),
        "case the two SET functions written out in full: expected the list appended and the \
         stored string kept, got {appended}"
    );
}
