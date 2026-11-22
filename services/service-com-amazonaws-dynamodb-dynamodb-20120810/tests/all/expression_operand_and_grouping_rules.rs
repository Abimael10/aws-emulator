// SPDX-License-Identifier: CC0-1.0

//! The rules an expression's OPERATORS are held to, and the ORDER the three rules AWS
//! states only of text its grammar accepts are answered in.
//!
//! The rules pinned here:
//!
//! 1. an operator or function whose FIRST operand names the same document location as one
//!    of its remaining operands relates a location to itself, which the expression's own
//!    text settles: it is refused naming the operator or function as the expression wrote
//!    it and that operand, with every `#alias` resolved. The rule is one rule over every
//!    operator and function that takes a first operand and remaining ones, so the table
//!    below spans the comparisons, the range and membership operators and the two
//!    functions that take two operands;
//! 2. a pair of parentheses that groups what is already grouped is refused in every
//!    grammar of this service that GROUPS with parentheses — the condition and filter
//!    language and the `KeyConditionExpression` one — and it is answered BEFORE the two
//!    rules about the expression's names and operands, which the same expression may also
//!    break;
//! 3. the documented quota on how many operators and functions one expression may state
//!    is reported with the count the expression STATED, whatever this emulator's own
//!    ceiling on the tokens it keeps.
//!
//! Each rule carries its CONTROL: two different locations are compared, a group that
//! carries a function call's arguments or one clause of a conjunction is not redundant,
//! and an expression inside the quota is served.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's
//! front doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses, with the composite key a key condition needs.
const TABLE: &str = "ExpressionOperandRules";

/// The primary key every row addresses.
fn key() -> Value {
    json!({"pk": {"S": "p"}, "sk": {"N": "1"}})
}

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write
/// one item, so a filter the service accepts has something to be evaluated against.
async fn seeded(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, "seed the table", TABLE).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "p"}, "sk": {"N": "1"},
                        "a": {"S": "x"}, "b": {"S": "x"},
                        "m": {"M": {"child": {"L": [{"S": "one"}, {"S": "two"}]}}}}}),
    )
    .await;
}

/// The clause a member's refusal carries, inside the `<N> validation error(s) detected: `
/// envelope for the members a WRITE states its intent through and on its own for the
/// members a READ narrows its result with.
fn stated_for(member: &str, clause: &str) -> String {
    let clause = format!("Invalid {member}: {clause}");
    if member == "ConditionExpression" || member == "UpdateExpression" {
        return format!("1 validation error detected: {clause}");
    }
    clause
}

/// The clause an operator relating one location to itself earns.
fn not_distinct(operator: &str, first_operand: &str) -> String {
    format!(
        "The first operand must be distinct from the remaining operands for this operator or \
         function; operator: {operator}, first operand: {first_operand}"
    )
}

/// One request that states an expression, and the whole message it earns.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    stated: String,
}

#[tokio::test]
async fn an_operator_whose_first_operand_is_not_distinct_is_refused() {
    let router = emulator();
    seeded(&router).await;

    // Rows differ in the OPERATOR OR FUNCTION — every comparison the language spells, the
    // range and membership operators, and the two functions whose second position takes a
    // location — in the shape of the first operand (a top-level attribute, a path two steps
    // inside the item, and two aliases that resolve to one attribute), and in the member
    // that carried the expression.
    let rows: Vec<Row> = vec![
        Row {
            case: "a filter asking whether an attribute equals itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a = a"}),
            stated: stated_for("FilterExpression", &not_distinct("=", "[a]")),
        },
        Row {
            case: "a filter asking whether an attribute differs from itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a <> a"}),
            stated: stated_for("FilterExpression", &not_distinct("<>", "[a]")),
        },
        Row {
            case: "a filter ordering an attribute below itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a < a"}),
            stated: stated_for("FilterExpression", &not_distinct("<", "[a]")),
        },
        Row {
            case: "a filter ordering an attribute at or below itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a <= a"}),
            stated: stated_for("FilterExpression", &not_distinct("<=", "[a]")),
        },
        Row {
            case: "a filter ordering an attribute above itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a > a"}),
            stated: stated_for("FilterExpression", &not_distinct(">", "[a]")),
        },
        Row {
            case: "a filter ordering an attribute at or above itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a >= a"}),
            stated: stated_for("FilterExpression", &not_distinct(">=", "[a]")),
        },
        Row {
            case: "a filter ranging an attribute from itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a BETWEEN a AND :v",
                            "ExpressionAttributeValues": {":v": {"S": "z"}}}),
            stated: stated_for("FilterExpression", &not_distinct("BETWEEN", "[a]")),
        },
        Row {
            case: "a filter asking whether an attribute is among values and itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "a IN (:v, a)",
                            "ExpressionAttributeValues": {":v": {"S": "z"}}}),
            stated: stated_for("FilterExpression", &not_distinct("IN", "[a]")),
        },
        Row {
            case: "a filter searching an attribute for itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "contains(a, a)"}),
            stated: stated_for("FilterExpression", &not_distinct("contains", "[a]")),
        },
        Row {
            case: "a filter testing an attribute for its own prefix",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(a, a)"}),
            stated: stated_for("FilterExpression", &not_distinct("begins_with", "[a]")),
        },
        Row {
            case: "a filter comparing one nested location with itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "m.child[1] = m.child[1]"}),
            stated: stated_for(
                "FilterExpression",
                &not_distinct("=", "[m, child, [1]]"),
            ),
        },
        Row {
            case: "a filter whose two aliases resolve to one attribute",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "#one = #two",
                            "ExpressionAttributeNames": {"#one": "a", "#two": "a"}}),
            stated: stated_for("FilterExpression", &not_distinct("=", "[a]")),
        },
        Row {
            case: "a query's filter comparing one attribute with itself",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "p"}},
                            "FilterExpression": "a = a"}),
            stated: stated_for("FilterExpression", &not_distinct("=", "[a]")),
        },
        Row {
            case: "a put's condition searching an aliased attribute for itself",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "contains(#one, #two)",
                            "ExpressionAttributeNames": {"#one": "a", "#two": "a"}}),
            stated: stated_for("ConditionExpression", &not_distinct("contains", "[a]")),
        },
        Row {
            case: "a delete's condition testing an attribute for its own prefix",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "begins_with(a, a)"}),
            stated: stated_for("ConditionExpression", &not_distinct("begins_with", "[a]")),
        },
        Row {
            case: "an update's condition comparing one attribute with itself",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET b = :v",
                            "ConditionExpression": "a = a",
                            "ExpressionAttributeValues": {":v": {"S": "y"}}}),
            stated: stated_for("ConditionExpression", &not_distinct("=", "[a]")),
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", row.stated.as_str()),
            "case {:?} on {}: expected the refusal {:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.stated
        );
    }

    // The CONTROL: the rule is about two operands naming ONE location, so an operator
    // relating two DIFFERENT locations — or a location and a value the request carried —
    // is evaluated against each item like any other.
    let served: Vec<(&str, Value)> = vec![
        (
            "two different attributes compared",
            json!({"TableName": TABLE, "FilterExpression": "a = b"}),
        ),
        (
            "one attribute searched for another",
            json!({"TableName": TABLE, "FilterExpression": "contains(a, b)"}),
        ),
        (
            "an attribute compared with a value the request carried",
            json!({"TableName": TABLE, "FilterExpression": "a = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
        ),
        (
            "two different elements of one list",
            json!({"TableName": TABLE, "FilterExpression": "m.child[0] = m.child[1]"}),
        ),
        (
            "two aliases resolving to different attributes",
            json!({"TableName": TABLE, "FilterExpression": "#one = #two",
                   "ExpressionAttributeNames": {"#one": "a", "#two": "b"}}),
        ),
    ];
    for (case, request) in &served {
        let (status, body) = call(&router, "Scan", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an operator relating two different locations is evaluated, got \
             {status} with {body}"
        );
    }
}

#[tokio::test]
async fn parentheses_that_group_what_is_already_grouped_outrank_the_rules_below_them() {
    let router = emulator();
    seeded(&router).await;

    let redundant = "The expression has redundant parentheses;";
    // Rows differ in the GRAMMAR that groups — the filter and condition language, and the
    // key-condition language, which tokenizes its own text — and in what ELSE the same
    // expression breaks: a reserved word written out as a name and an operator relating one
    // location to itself are each the rule the expression would be refused for if its
    // parentheses said something.
    let rows: Vec<Row> = vec![
        Row {
            case: "a filter wrapping a group in another group",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "((attribute_exists(a)))"}),
            stated: stated_for("FilterExpression", redundant),
        },
        Row {
            case: "a filter whose redundant group also writes a reserved word",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "((status = :v))",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: stated_for("FilterExpression", redundant),
        },
        Row {
            case: "a filter whose redundant group also compares one attribute with itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "((a = a))"}),
            stated: stated_for("FilterExpression", redundant),
        },
        Row {
            case: "a write's condition wrapping a group in another group",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "((attribute_not_exists(a)))"}),
            stated: stated_for("ConditionExpression", redundant),
        },
        Row {
            case: "a key condition wrapping a group in another group",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "((pk = :p))",
                            "ExpressionAttributeValues": {":p": {"S": "p"}}}),
            stated: stated_for("KeyConditionExpression", redundant),
        },
        Row {
            case: "a key condition whose second clause is grouped twice",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p AND ((sk > :s))",
                            "ExpressionAttributeValues": {":p": {"S": "p"},
                                                          ":s": {"N": "0"}}}),
            stated: stated_for("KeyConditionExpression", redundant),
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", row.stated.as_str()),
            "case {:?} on {}: expected the refusal {:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.stated
        );
    }

    // The CONTROL: a group that carries a call's arguments, and one that groups a clause of
    // a conjunction, each say what the expression means — neither is redundant.
    let served: Vec<(&str, &str, Value)> = vec![
        (
            "a group around a whole function call",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "(attribute_exists(a))"}),
        ),
        (
            "a group around each clause of a filter",
            "Scan",
            json!({"TableName": TABLE,
                   "FilterExpression": "(attribute_exists(a)) AND (attribute_exists(b))"}),
        ),
        (
            "a group around a whole key condition",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "(pk = :p)",
                   "ExpressionAttributeValues": {":p": {"S": "p"}}}),
        ),
        (
            "a group around each clause of a key condition",
            "Query",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "(pk = :p) AND (sk > :s)",
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":s": {"N": "0"}}}),
        ),
    ];
    for (case, operation, request) in &served {
        let (status, body) = call(&router, operation, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?} on {operation}: a group that says something is served, got {status} \
             with {body}"
        );
    }
}

/// One expression stating `comparisons` comparisons joined by `AND`, which states
/// `2 * comparisons - 1` operators: one per comparison and one per connective.
fn conjunction_of(comparisons: usize) -> String {
    (0..comparisons)
        .map(|_| "a = :v".to_string())
        .collect::<Vec<String>>()
        .join(" AND ")
}

#[tokio::test]
async fn the_operator_quota_reports_the_count_the_expression_stated() {
    let router = emulator();
    seeded(&router).await;

    // The documented quota is 300 operators or functions. Rows differ in how far past it
    // the expression goes, and so in whether the token stream is longer than the one this
    // emulator keeps: the quota is the rule the caller is answered by either way, and the
    // count it reports is the one the expression stated.
    let rows: Vec<Row> = vec![
        Row {
            case: "a filter one operator past the quota",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": conjunction_of(151),
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: stated_for(
                "FilterExpression",
                "The expression contains too many operators; operator count: 301",
            ),
        },
        Row {
            case: "a filter past the quota and past the tokens this service keeps",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": conjunction_of(342),
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: stated_for(
                "FilterExpression",
                "The expression contains too many operators; operator count: 683",
            ),
        },
        Row {
            case: "a write's condition past the quota and past those tokens",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": conjunction_of(342),
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: stated_for(
                "ConditionExpression",
                "The expression contains too many operators; operator count: 683",
            ),
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", row.stated.as_str()),
            "case {:?} on {}: expected the refusal {:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.stated
        );
    }

    // The CONTROL: an expression at the quota is served — 150 comparisons state 299
    // operators, one short of it, and 151 comparisons were refused above.
    let (status, body) = call(
        &router,
        "Scan",
        json!({"TableName": TABLE,
               "FilterExpression": conjunction_of(150),
               "ExpressionAttributeValues": {":v": {"S": "x"}}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case an expression inside the operator quota: it is served, got {status} with {body}"
    );
}
