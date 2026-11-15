// SPDX-License-Identifier: CC0-1.0

//! WHERE an expression refusal says the parse stopped.
//!
//! The rule pinned here: a request whose expression the grammar cannot read is refused
//! naming the FIRST token the parse could not consume — `Syntax error; token: "<token>",
//! near: "<the expression's own text around it>"` — never the token after it. A parse
//! position that cannot accept the token it is at has not consumed it, so that token, and
//! the slice of the submitted text around it, is what points a caller (or a tool
//! correcting an expression programmatically) at the defect.
//!
//! The rows differ along what the rule generalises over: the expression MEMBER the text
//! arrived in (`ProjectionExpression`, `FilterExpression`, `ConditionExpression`,
//! `UpdateExpression`), the operation that carried it (a keyed read, both table walks and
//! all three single-item writes), WHICH grammar position the parse stopped at (a document
//! path's next step, the operator of a comparison, the value operand of an `ADD` or a
//! `DELETE`), WHERE in the expression the offending token sits (the first path step, a
//! step further in, a later path of a list), and whether the table the
//! request addressed exists at all — validation of the request precedes the table lookup,
//! so the same text earns the same message either way.
//!
//! Which refusals carry the `<N> validation error(s) detected: ` envelope is this
//! service's existing split and is not what this suite is about: the members a WRITE
//! states its intent through carry it and the members a READ narrows its result with do
//! not, so each row states the whole message it must earn.
//!
//! Every call is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses, except the row that names one no region holds.
const TABLE: &str = "SyntaxPositions";

/// A table name no call of this suite creates.
const ABSENT: &str = "NoSuchSyntaxTable";

/// One emulator holding one active table keyed on `pk`, with one item in it.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the syntax-position table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "one"}, "a": {"S": "x"},
                        "m": {"M": {"leaf": {"S": "x"}}}}}),
    )
    .await;
    router
}

/// The clause a syntax error states, over the member it was found in.
fn syntax_clause(member: &str, token: &str, near: &str) -> String {
    format!("Invalid {member}: Syntax error; token: \"{token}\", near: \"{near}\"")
}

/// The same clause inside the envelope the write members state their refusals in.
fn enveloped(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

#[tokio::test]
async fn a_syntax_error_names_the_first_token_the_parse_could_not_consume() {
    let router = seeded().await;

    // (case, operation, request, the whole message it must earn)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a doubled separator in the only projected path of a keyed read",
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "a..b"}),
            syntax_clause("ProjectionExpression", ".", "..b"),
        ),
        (
            // The same text on a table NO region holds: the request is refused for what
            // it says, not for what it addresses.
            "the same projection against a table that does not exist",
            "GetItem",
            json!({"TableName": ABSENT, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "a..b"}),
            syntax_clause("ProjectionExpression", ".", "..b"),
        ),
        (
            "a doubled separator deeper inside a projected path",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "a.b..c"}),
            syntax_clause("ProjectionExpression", ".", "..c"),
        ),
        (
            "a doubled separator in the SECOND path of a projection list",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "one"}},
                   "ProjectionExpression": "a, b..c"}),
            syntax_clause("ProjectionExpression", ".", "..c"),
        ),
        (
            "a doubled separator in a filter's left operand",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "a..b = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            syntax_clause("FilterExpression", ".", "..b"),
        ),
        (
            "a doubled separator inside a function's argument of a filter",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "one"}},
                   "FilterExpression": "attribute_exists(a..b)"}),
            syntax_clause("FilterExpression", ".", "..b"),
        ),
        (
            "a doubled separator in the condition a put states",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "two"}},
                   "ConditionExpression": "attribute_not_exists(a..b)"}),
            enveloped(&syntax_clause("ConditionExpression", ".", "..b")),
        ),
        (
            "a doubled separator in the condition a delete states",
            "DeleteItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ConditionExpression": "p..q = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            enveloped(&syntax_clause("ConditionExpression", ".", "..q")),
        ),
        (
            "a doubled separator in the path an update writes",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "SET a..b = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            enveloped(&syntax_clause("UpdateExpression", ".", "..b")),
        ),
        (
            "a doubled separator in the path an update removes",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "REMOVE a.b..c"}),
            enveloped(&syntax_clause("UpdateExpression", ".", "..c")),
        ),
        (
            // The comparison position: a filter that names two paths where the
            // grammar needs an operator between them stops at the SECOND path, which
            // is the first token that position cannot take.
            "a filter whose comparison carries no operator",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "a b"}),
            syntax_clause("FilterExpression", "b", "a b"),
        ),
        (
            "a query filter whose comparison carries no operator",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "one"}},
                   "FilterExpression": "attribute_exists(a) AND b c"}),
            syntax_clause("FilterExpression", "c", "b c"),
        ),
        (
            // The value-operand position: `ADD` and `DELETE` take a `:placeholder`,
            // so a path written where the value belongs is where the parse stops.
            "an update whose ADD names a path where a value belongs",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "ADD a b"}),
            enveloped(&syntax_clause("UpdateExpression", "b", "a b")),
        ),
        (
            "an update whose DELETE names a path where a value belongs",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "DELETE a b"}),
            enveloped(&syntax_clause("UpdateExpression", "b", "a b")),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, answered) = call(&router, operation, request.clone()).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?} ({operation} {request}): expected {expected:?}, got {status} with \
             {answered}"
        );
    }
}

#[tokio::test]
async fn an_expression_the_grammar_reads_is_served() {
    let router = seeded().await;

    // The SAME paths written with one separator are ordinary document paths, so the rows
    // above are the doubled separator being refused rather than the path being rejected.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "ProjectionExpression": "a, pk"}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "one"}, "a": {"S": "x"}}),
        "case a projection of two single-step paths: expected both attributes, got {read}"
    );
    let updated = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "UpdateExpression": "SET m.leaf = :v",
               "ExpressionAttributeValues": {":v": {"S": "y"}},
               "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(
        updated["Attributes"]["m"],
        json!({"M": {"leaf": {"S": "y"}}}),
        "case an update of a two-step path: expected the nested value written, got {updated}"
    );
}
