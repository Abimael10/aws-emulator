// SPDX-License-Identifier: CC0-1.0

//! Where a FUNCTION CALL may stand in an expression, and what a call written anywhere else
//! is refused for.
//!
//! RULE: a function's operand states a value or the document path a value is read from —
//! "if_not_exists (path, operand)" and "list_append (operand, operand)"
//! (`UpdateItem$UpdateExpression`), "attribute_exists (path)", "size (path)" and the rest
//! of the `ConditionExpression` function reference — so a CALL written in one of those
//! positions is a function used where the grammar states none, and the expression is
//! refused rather than evaluated:
//!
//! * a position the signature declares as a PATH earns "Operator or function requires a
//!   document path; operator or function: <the function whose position it is>", the same
//!   clause a `:placeholder` written there earns, because neither names a location;
//! * any other operand position earns "The function is not allowed to be used this way in
//!   an expression; function: <the function used that way>", which names the call that is
//!   misplaced rather than reporting its name as unknown — the grammar knows it.
//!
//! The ONE nesting the update-expression reference writes is `list_append`'s operand read
//! through `if_not_exists`, which fills a list that may not be there yet; that form is
//! served and its result is what the two functions compose.
//!
//! The rows span the function at fault (`if_not_exists`, `list_append`, `size`,
//! `attribute_exists`), the POSITION it was written in (a path position, a value position,
//! either operand of a two-operand call), the member that carried the text
//! (`UpdateExpression`, `ConditionExpression`, `FilterExpression` — which decides whether
//! the clause carries the validation envelope) and the operation that read it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: one string partition key.
const TABLE: &str = "FunctionPositions";

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
            "l": {"L": [{"S": "one"}]},
            "s": {"S": "beta"},
            "n": {"N": "1"},
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

/// The whole message a call written where a PATH belongs earns.
fn needs_a_path(member: &str, function: &str) -> String {
    clause(
        member,
        &format!(
            "Operator or function requires a document path; operator or function: {function}"
        ),
    )
}

/// The whole message a call written in any other operand position earns.
fn not_this_way(member: &str, function: &str) -> String {
    clause(
        member,
        &format!(
            "The function is not allowed to be used this way in an expression; function: \
             {function}"
        ),
    )
}

/// One row: the case, the operation, the request, and the whole message it must earn.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected: String,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // `if_not_exists`'s first operand is the location it reads, so a call written
            // there names none.
            case: "an if_not_exists nested as its own first operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = if_not_exists(if_not_exists(s, :a), :b)",
                            "ExpressionAttributeValues": {":a": {"S": "x"}, ":b": {"S": "y"}}}),
            expected: enveloped(&needs_a_path("UpdateExpression", "if_not_exists")),
        },
        Row {
            case: "a list_append nested as if_not_exists' first operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression":
                                "SET l = if_not_exists(list_append(l, :more), :empty)",
                            "ExpressionAttributeValues": {":more": {"L": [{"S": "two"}]},
                                                          ":empty": {"L": []}}}),
            expected: enveloped(&needs_a_path("UpdateExpression", "if_not_exists")),
        },
        Row {
            // Its SECOND operand is the value it falls back to, which a call is not.
            case: "an if_not_exists nested as its own fallback operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = if_not_exists(s, if_not_exists(n, :a))",
                            "ExpressionAttributeValues": {":a": {"S": "x"}}}),
            expected: enveloped(&not_this_way("UpdateExpression", "if_not_exists")),
        },
        Row {
            case: "a list_append nested as if_not_exists' fallback operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression":
                                "SET l = if_not_exists(l, list_append(l, :more))",
                            "ExpressionAttributeValues": {":more": {"L": [{"S": "two"}]}}}),
            expected: enveloped(&not_this_way("UpdateExpression", "list_append")),
        },
        Row {
            case: "a list_append nested as its own first operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression":
                                "SET l = list_append(list_append(l, :more), :more)",
                            "ExpressionAttributeValues": {":more": {"L": [{"S": "two"}]}}}),
            expected: enveloped(&not_this_way("UpdateExpression", "list_append")),
        },
        Row {
            case: "a list_append nested as its own second operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression":
                                "SET l = list_append(:more, list_append(l, :more))",
                            "ExpressionAttributeValues": {":more": {"L": [{"S": "two"}]}}}),
            expected: enveloped(&not_this_way("UpdateExpression", "list_append")),
        },
        Row {
            // The same expression inside a TRANSACTION is read by the same parser, and an
            // element's own members state the clause without the envelope.
            case: "a transaction element's update nesting list_append in itself",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [{"Update": {
                "TableName": TABLE, "Key": key(),
                "UpdateExpression": "SET l = list_append(list_append(l, :more), :more)",
                "ExpressionAttributeValues": {":more": {"L": [{"S": "two"}]}},
            }}]}),
            expected: not_this_way("UpdateExpression", "list_append"),
        },
        Row {
            // A CONDITION's functions read a path first, so a call written there names no
            // location either — whichever function the call is.
            case: "a condition calling size where attribute_exists reads a path",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_exists(size(s))"}),
            expected: enveloped(&needs_a_path("ConditionExpression", "attribute_exists")),
        },
        Row {
            case: "a filter calling size inside size",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "size(size(s)) = :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: needs_a_path("FilterExpression", "size"),
        },
    ]
}

#[tokio::test]
async fn a_call_written_in_an_operand_position_is_refused_for_that_position() {
    let router = seeded().await;

    for Row {
        case,
        operation,
        request,
        expected,
    } in rows()
    {
        let (status, answered) = call(&router, operation, request.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({operation} {request}): expected {expected:?}, got {status} \
             with {answered}"
        );
    }

    // Nothing any refused row asked for was written: the item is exactly as it was seeded.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key(), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        (read["Item"]["l"].clone(), read["Item"]["s"].clone()),
        (json!({"L": [{"S": "one"}]}), json!({"S": "beta"})),
        "case a refused call position: the seeded item is untouched, got {read}"
    );
}

#[tokio::test]
async fn the_nesting_the_update_reference_writes_is_served_and_composes() {
    let router = seeded().await;

    // `list_append`'s operand read through `if_not_exists` is the form the reference
    // writes for appending to a list that may not be there yet. Both readings of it are
    // driven: over the list the item HOLDS, and over an attribute it does not carry.
    let appended = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET l = list_append(if_not_exists(l, :empty), :more)",
               "ExpressionAttributeValues": {":empty": {"L": []},
                                             ":more": {"L": [{"S": "two"}]}},
               "ReturnValues": "UPDATED_NEW"}),
    )
    .await;
    assert_eq!(
        appended["Attributes"]["l"],
        json!({"L": [{"S": "one"}, {"S": "two"}]}),
        "case appending through if_not_exists: expected the stored list extended, got \
         {appended}"
    );

    let created = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET fresh = list_append(if_not_exists(fresh, :empty), :more)",
               "ExpressionAttributeValues": {":empty": {"L": []},
                                             ":more": {"L": [{"S": "two"}]}},
               "ReturnValues": "UPDATED_NEW"}),
    )
    .await;
    assert_eq!(
        created["Attributes"]["fresh"],
        json!({"L": [{"S": "two"}]}),
        "case appending to an attribute that is not there: expected the fallback list \
         extended, got {created}"
    );

    // Both landed in the store, read back through the implemented reader.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": key(), "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        (read["Item"]["l"].clone(), read["Item"]["fresh"].clone()),
        (
            json!({"L": [{"S": "one"}, {"S": "two"}]}),
            json!({"L": [{"S": "two"}]})
        ),
        "case the served nesting: expected both lists stored, got {read}"
    );
}
