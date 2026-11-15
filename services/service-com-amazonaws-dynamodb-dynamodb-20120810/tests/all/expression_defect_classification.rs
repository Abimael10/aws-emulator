// SPDX-License-Identifier: CC0-1.0

//! WHICH CLASS a defect of an expression belongs to, driven end to end.
//!
//! RULE under test: a defect of the EXPRESSION ITSELF — a literal operand of a type the
//! operator or function it was written with cannot take, a call carrying the wrong number
//! of operands, a value where the signature declares a document path, a range whose bounds
//! are the wrong way round, a member carrying no expression at all, a key condition
//! doubling one key attribute, a substitution-table key that is no substitution token — is
//! a defect of the REQUEST. Every one of them is settled from the request's own text,
//! before the addressed item is read, and answered 400 `ValidationException` naming the
//! rule that was broken: never `ConditionalCheckFailedException`, never a cancelled
//! transaction, never a 200 selecting nothing, and never the parser's token dump.
//!
//! The rows differ along what the rule generalises over: the expression MEMBER the text
//! arrived in (`ConditionExpression`, `FilterExpression`, `UpdateExpression`,
//! `KeyConditionExpression`), the operation carrying it (both table walks, all three
//! single-item writes, a transaction), whether the member is the request's own or one
//! element's, the operator or function at fault (`<`, `<=`, `>`, `>=`, `BETWEEN`,
//! `attribute_exists`, `attribute_not_exists`, `attribute_type`, `begins_with`,
//! `contains`, `size`, `if_not_exists`, `list_append`), the TYPE of the offending literal
//! (`BOOL`, `NULL`, `L`, `M`, `SS`, `N`, a text that denotes no number) and how many
//! operands a call carried (none, one, two, three).
//!
//! Which refusals carry the `<N> validation error(s) detected: ` envelope is this service's
//! existing split (the members a WRITE states its intent through carry it, the members a
//! READ narrows its result with do not, and an ELEMENT's members never do), so each row
//! states the whole message it must earn.
//!
//! The two control suites below drive the SAME shapes with operands their operator does
//! take: those are evaluated against the stored item, so the refusals above are this
//! classification and not a blanket rejection.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row addresses: a string partition key and a string sort key, so the
/// key-condition rows have both halves of a key to speak about.
const TABLE: &str = "ExpressionDefectClasses";

/// The envelope a refusal about the REQUEST's own write-intent members is stated inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The key of the one seeded item, which the write rows address.
fn key() -> Value {
    json!({"pk": {"S": "item-1"}, "sk": {"S": "a"}})
}

/// The emulator with the table and one item in place, so no row is refused for its table
/// or its key instead of for the rule under test.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "item-1"}, "sk": {"S": "a"},
                        "n": {"N": "3"}, "s": {"S": "beta"},
                        "peer": {"S": "beta"}, "tags": {"SS": ["red"]}}}),
    )
    .await;
    router
}

/// One row: the case, the operation, the request, and the whole message it must earn.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected: String,
}

/// The clause a refusal about `member` states, as that member's refusals are worded.
fn clause(member: &str, detail: &str) -> String {
    format!("Invalid {member}: {detail}")
}

/// The same clause inside the envelope the write-intent members state their refusals in.
fn enveloped(clause: &str) -> String {
    format!("{ENVELOPE}{clause}")
}

/// The detail an operand of a type its operator or function does not take earns.
fn operand_type(operator_or_function: &str, datatype: &str) -> String {
    format!(
        "Incorrect operand type for operator or function; operator or function: \
         {operator_or_function}, operand type: {datatype}"
    )
}

/// The detail a call carrying the wrong number of operands earns.
fn operand_count(operator_or_function: &str, operands: usize) -> String {
    format!(
        "Incorrect number of operands for operator or function; operator or function: \
         {operator_or_function}, number of operands: {operands}"
    )
}

/// The detail a value written where the signature declares a document path earns.
fn needs_a_path(operator_or_function: &str) -> String {
    format!(
        "Operator or function requires a document path; operator or function: \
         {operator_or_function}"
    )
}

/// The detail a range whose bounds are the wrong way round earns, echoing both as the
/// operands the request stated.
fn reversed_bounds(low: &str, high: &str) -> String {
    format!(
        "The BETWEEN operator requires upper bound to be greater than or equal to lower \
         bound; lower bound operand: AttributeValue: {{{low}}}, upper bound operand: \
         AttributeValue: {{{high}}}"
    )
}

/// The rows where an ORDERING comparison is given a literal of a type DynamoDB does not
/// order: one per comparison operator, per non-orderable type, per member and per
/// operation that carries one.
fn ordering_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a put's condition comparing below a BOOL",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "n < :v",
                            "ExpressionAttributeValues": {":v": {"BOOL": true}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_type("<", "BOOL"),
            )),
        },
        Row {
            case: "a put's condition comparing at most a NULL",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "n <= :v",
                            "ExpressionAttributeValues": {":v": {"NULL": true}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_type("<=", "NULL"),
            )),
        },
        Row {
            case: "a delete's condition comparing above a list",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "n > :v",
                            "ExpressionAttributeValues": {":v": {"L": [{"N": "1"}]}}}),
            expected: enveloped(&clause("ConditionExpression", &operand_type(">", "L"))),
        },
        Row {
            case: "an update's condition comparing at least a map",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :new",
                            "ConditionExpression": "n >= :v",
                            "ExpressionAttributeValues": {":v": {"M": {"a": {"N": "1"}}},
                                                          ":new": {"S": "gamma"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_type(">=", "M"),
            )),
        },
        Row {
            case: "a scan's filter comparing below a string set",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "n < :v",
                            "ExpressionAttributeValues": {":v": {"SS": ["red"]}}}),
            expected: clause("FilterExpression", &operand_type("<", "SS")),
        },
        Row {
            case: "a query's filter comparing above a BOOL",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "n > :v",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"},
                                                          ":v": {"BOOL": false}}}),
            expected: clause("FilterExpression", &operand_type(">", "BOOL")),
        },
        Row {
            case: "one transacted put's condition comparing above a BOOL",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [{"Put": {
                "TableName": TABLE,
                "Item": key(),
                "ConditionExpression": "n > :v",
                "ExpressionAttributeValues": {":v": {"BOOL": true}}}}]}),
            expected: clause("ConditionExpression", &operand_type(">", "BOOL")),
        },
        Row {
            case: "a put's condition ranging over a BOOL bound",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "n BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":lo": {"BOOL": true},
                                                          ":hi": {"N": "9"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_type("BETWEEN", "BOOL"),
            )),
        },
        Row {
            case: "a scan's filter ranging over a map bound",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "n BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":lo": {"N": "1"},
                                                          ":hi": {"M": {}}}}),
            expected: clause("FilterExpression", &operand_type("BETWEEN", "M")),
        },
        Row {
            case: "a put's condition comparing against a text that denotes no number",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "n < :v",
                            "ExpressionAttributeValues": {":v": {"N": "abc"}}}),
            expected: format!(
                "{ENVELOPE}The parameter cannot be converted to a numeric value: abc"
            ),
        },
    ]
}

/// The rows where a range states its two bounds the wrong way round: the bounds are
/// literals, so the order is a fact of the request.
fn reversed_bound_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a put's condition ranging from a higher number to a lower one",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "n BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":lo": {"N": "5"},
                                                          ":hi": {"N": "1"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &reversed_bounds("N:5", "N:1"),
            )),
        },
        Row {
            case: "a delete's condition ranging from a later string to an earlier one",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "s BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":lo": {"S": "b"},
                                                          ":hi": {"S": "a"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &reversed_bounds("S:b", "S:a"),
            )),
        },
        Row {
            case: "a scan's filter ranging the wrong way round",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "n BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":lo": {"N": "10"},
                                                          ":hi": {"N": "2"}}}),
            expected: clause("FilterExpression", &reversed_bounds("N:10", "N:2")),
        },
        Row {
            case: "a query's key condition ranging the wrong way round",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p AND sk BETWEEN :lo AND :hi",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"},
                                                          ":lo": {"S": "b"},
                                                          ":hi": {"S": "a"}}}),
            expected: clause("KeyConditionExpression", &reversed_bounds("S:b", "S:a")),
        },
    ]
}

/// The rows where a CALL does not match the signature of the function it names: the wrong
/// number of operands, a value where a path belongs, a path where a value belongs, a
/// literal outside the closed domain the function's own operand has.
fn call_shape_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a filter asking whether a value placeholder exists",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_exists(:v)",
                            "ExpressionAttributeValues": {":v": {"S": "beta"}}}),
            expected: clause("FilterExpression", &needs_a_path("attribute_exists")),
        },
        Row {
            case: "a filter giving attribute_exists two operands",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_exists(#a, #a)",
                            "ExpressionAttributeNames": {"#a": "n"}}),
            expected: clause("FilterExpression", &operand_count("attribute_exists", 2)),
        },
        Row {
            case: "a filter giving attribute_not_exists no operand",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_not_exists()"}),
            expected: clause("FilterExpression", &operand_count("attribute_not_exists", 0)),
        },
        Row {
            case: "a filter giving contains three operands",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "contains(tags, :a, :b)",
                            "ExpressionAttributeValues": {":a": {"S": "red"},
                                                          ":b": {"S": "blue"}}}),
            expected: clause("FilterExpression", &operand_count("contains", 3)),
        },
        Row {
            case: "a filter asking contains to search for another attribute",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "contains(s, peer)"}),
            expected: clause("FilterExpression", &operand_type("contains", "PATH")),
        },
        Row {
            case: "a write's condition asking contains to search for another attribute",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "contains(tags, tags)"}),
            expected: enveloped(&clause(
                "ConditionExpression",
                &operand_type("contains", "PATH"),
            )),
        },
        Row {
            case: "a filter whose begins_with prefix is a number",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(s, :v)",
                            "ExpressionAttributeValues": {":v": {"N": "1"}}}),
            expected: clause("FilterExpression", &operand_type("begins_with", "N")),
        },
        Row {
            case: "a filter whose begins_with prefix is another attribute",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "begins_with(s, peer)"}),
            expected: clause("FilterExpression", &operand_type("begins_with", "PATH")),
        },
        Row {
            case: "a filter naming a type attribute_type does not have",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_type(n, :t)",
                            "ExpressionAttributeValues": {":t": {"S": "NUMBER"}}}),
            expected: clause(
                "FilterExpression",
                "Invalid attribute type name found; type: NUMBER, valid types: \
                 {S,SS,N,NS,B,BS,BOOL,NULL,L,M}",
            ),
        },
        Row {
            case: "a write's condition naming a type attribute_type does not have",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_type(n, :t)",
                            "ExpressionAttributeValues": {":t": {"S": "s"}}}),
            expected: enveloped(&clause(
                "ConditionExpression",
                "Invalid attribute type name found; type: s, valid types: \
                 {S,SS,N,NS,B,BS,BOOL,NULL,L,M}",
            )),
        },
        Row {
            case: "a filter taking the size of two paths",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "size(#a, #b) > :n",
                            "ExpressionAttributeNames": {"#a": "s", "#b": "peer"},
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: clause("FilterExpression", &operand_count("size", 2)),
        },
        Row {
            case: "a filter taking the size of a value placeholder",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "size(:v) > :n",
                            "ExpressionAttributeValues": {":v": {"S": "beta"},
                                                          ":n": {"N": "1"}}}),
            expected: clause("FilterExpression", &needs_a_path("size")),
        },
        Row {
            case: "an update defaulting a value placeholder rather than a path",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = if_not_exists(:v, :w)",
                            "ExpressionAttributeValues": {":v": {"S": "beta"},
                                                          ":w": {"S": "gamma"}}}),
            expected: enveloped(&clause(
                "UpdateExpression",
                &needs_a_path("if_not_exists"),
            )),
        },
        Row {
            case: "an update appending with one operand",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = list_append(:v)",
                            "ExpressionAttributeValues": {":v": {"L": [{"S": "x"}]}}}),
            expected: enveloped(&clause(
                "UpdateExpression",
                &operand_count("list_append", 1),
            )),
        },
    ]
}

/// The rows where an expression member carries no expression at all: the member is the
/// mistake, so the refusal states that rather than pointing at a token there is none of.
fn empty_expression_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a scan's empty filter",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": ""}),
            expected: clause("FilterExpression", "The expression can not be empty;"),
        },
        Row {
            case: "a scan's blank filter",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "   "}),
            expected: clause("FilterExpression", "The expression can not be empty;"),
        },
        Row {
            case: "a put's empty condition",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(), "ConditionExpression": ""}),
            expected: enveloped(&clause(
                "ConditionExpression",
                "The expression can not be empty;",
            )),
        },
        Row {
            case: "a query's empty key condition",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": ""}),
            expected: clause(
                "KeyConditionExpression",
                "The expression can not be empty;",
            ),
        },
        Row {
            case: "one transacted condition check's empty condition",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [{"ConditionCheck": {
                "TableName": TABLE, "Key": key(), "ConditionExpression": ""}}]}),
            expected: clause("ConditionExpression", "The expression can not be empty;"),
        },
    ]
}

/// The rows about a KEY condition's own rules: one clause per key attribute, and text the
/// member's grammar cannot read refused as the key-condition defect it is.
fn key_condition_rows() -> Vec<Row> {
    vec![
        Row {
            case: "two clauses on the partition key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p AND pk = :q",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"},
                                                          ":q": {"S": "item-2"}}}),
            expected: clause(
                "KeyConditionExpression",
                "KeyConditionExpressions must only contain one condition per key",
            ),
        },
        Row {
            case: "two clauses on the sort key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "sk = :a AND sk = :b",
                            "ExpressionAttributeValues": {":a": {"S": "a"},
                                                          ":b": {"S": "b"}}}),
            expected: clause(
                "KeyConditionExpression",
                "KeyConditionExpressions must only contain one condition per key",
            ),
        },
        Row {
            case: "a document path where a key attribute belongs",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk.x = :p",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"}}}),
            expected: clause(
                "KeyConditionExpression",
                "Syntax error; token: \".\", near: \"pk.x\"",
            ),
        },
        Row {
            case: "a hyphenated name where a key attribute belongs",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk-name = :p",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"}}}),
            expected: clause(
                "KeyConditionExpression",
                "Syntax error; token: \"-\", near: \"pk-name\"",
            ),
        },
        Row {
            case: "a sort-key prefix compared against a number",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p AND begins_with(sk, :n)",
                            "ExpressionAttributeValues": {":p": {"S": "item-1"},
                                                          ":n": {"N": "1"}}}),
            expected: clause(
                "KeyConditionExpression",
                &operand_type("begins_with", "N"),
            ),
        },
    ]
}

/// The rows about a substitution table's KEYS: a key that is not spelled as the token an
/// expression writes for it is that member's own defect.
fn substitution_key_rows() -> Vec<Row> {
    let invalid = |member: &str, key: &str| {
        format!("{member} contains invalid key: Syntax error; key: \"{key}\"")
    };
    vec![
        Row {
            case: "a name key with no # at all",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_not_exists(#a)",
                            "ExpressionAttributeNames": {"a": "n"}}),
            expected: invalid("ExpressionAttributeNames", "a"),
        },
        Row {
            case: "a name key that is nothing but its #",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "#a",
                            "ExpressionAttributeNames": {"#": "n"}}),
            expected: invalid("ExpressionAttributeNames", "#"),
        },
        Row {
            case: "a name key carrying a character no expression can write",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "attribute_exists(#a)",
                            "ExpressionAttributeNames": {"#a-b": "n"}}),
            expected: invalid("ExpressionAttributeNames", "#a-b"),
        },
        Row {
            case: "a value key with no colon at all",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "n = :v",
                            "ExpressionAttributeValues": {"v": {"N": "3"}}}),
            expected: invalid("ExpressionAttributeValues", "v"),
        },
        Row {
            case: "a value key carrying a document-path separator",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p.q": {"S": "item-1"}}}),
            expected: invalid("ExpressionAttributeValues", ":p.q"),
        },
        Row {
            case: "a value key on one transacted put",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [{"Put": {
                "TableName": TABLE, "Item": key(),
                "ConditionExpression": "n = :v",
                "ExpressionAttributeValues": {"v": {"N": "3"}}}}]}),
            expected: invalid("ExpressionAttributeValues", "v"),
        },
    ]
}

/// Every key the rows address, as it stands — for the assertion that a refused request
/// changed nothing.
async fn stored(router: &MultiServiceRouter) -> Value {
    ok(router, "GetItem", json!({"TableName": TABLE, "Key": key()}))
        .await
        .get("Item")
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_defect_of_the_expression_is_a_validation_exception_naming_the_rule() {
    let router = seeded().await;
    let before = stored(&router).await;

    for row in ordering_rows()
        .into_iter()
        .chain(reversed_bound_rows())
        .chain(call_shape_rows())
        .chain(empty_expression_rows())
        .chain(key_condition_rows())
        .chain(substitution_key_rows())
    {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {:?} ({}): expected 400 ValidationException, got {status} with {body}",
            row.case,
            row.operation
        );
        let stated = message(&body);
        assert_eq!(
            stated, row.expected,
            "case {:?} ({}): expected {:?}, got {stated:?}",
            row.case, row.operation, row.expected
        );
    }

    // None of those requests was evaluated against the stored item, so none of them wrote,
    // replaced or deleted it.
    let after = stored(&router).await;
    assert_eq!(
        after, before,
        "a refused request must change nothing: expected {before:?}, got {after:?}"
    );
}

#[tokio::test]
async fn the_same_shapes_with_operands_their_operator_takes_are_evaluated() {
    let router = seeded().await;

    // A condition whose operands the comparison DOES take is read against the stored item:
    // the item carries `n = 3`, so this one does not hold and the write is refused as the
    // optimistic-concurrency outcome it is — a different error class from every row above.
    let (status, body) = call(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": key(), "ConditionExpression": "n < :v",
               "ExpressionAttributeValues": {":v": {"N": "1"}}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ConditionalCheckFailedException"),
        "case an unmet comparison on a number: expected the condition to be evaluated, got \
         {status} with {body}"
    );

    // And the transaction form of the same shape is cancelled rather than refused.
    let (status, body) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Put": {
            "TableName": TABLE, "Item": key(), "ConditionExpression": "n > :v",
            "ExpressionAttributeValues": {":v": {"N": "9"}}}}]}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "TransactionCanceledException"),
        "case an unmet transacted comparison: expected the condition to be evaluated, got \
         {status} with {body}"
    );

    // Every filter and function form above, with the operand its signature declares: each
    // is evaluated and selects the seeded item.
    for (case, filter, values) in [
        ("a comparison on a number", "n < :v", json!({":v": {"N": "9"}})),
        (
            "a range the right way round",
            "n BETWEEN :lo AND :hi",
            json!({":lo": {"N": "1"}, ":hi": {"N": "5"}}),
        ),
        (
            "a string prefix",
            "begins_with(s, :v)",
            json!({":v": {"S": "bet"}}),
        ),
        (
            "a type test naming a type",
            "attribute_type(n, :t)",
            json!({":t": {"S": "N"}}),
        ),
        (
            "a set membership test",
            "contains(tags, :m)",
            json!({":m": {"S": "red"}}),
        ),
        (
            "a size comparison",
            "size(s) = :n",
            json!({":n": {"N": "4"}}),
        ),
    ] {
        let answered = ok(
            &router,
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": filter,
                   "ExpressionAttributeValues": values}),
        )
        .await;
        assert_eq!(
            answered["Count"],
            json!(1),
            "case {case:?}: expected the seeded item to be selected, got {answered}"
        );
    }

    // A key condition the request wrote OPERAND FIRST is a legal sort-key comparison, and
    // one clause per key attribute is what the rule above refuses two of.
    let answered = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "KeyConditionExpression": "pk = :p AND :lo <= sk",
               "ExpressionAttributeValues": {":p": {"S": "item-1"}, ":lo": {"S": "a"}}}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case a value-first sort-key comparison: expected the seeded item, got {answered}"
    );

    // And the substitution tables whose keys ARE tokens are served, in both members.
    let answered = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "#a = :v",
               "ExpressionAttributeNames": {"#a": "n"},
               "ExpressionAttributeValues": {":v": {"N": "3"}}}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case well-formed substitution keys: expected the seeded item, got {answered}"
    );
}
