// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a FUNCTION CALL whose first operand names the same document location as
//! one of its remaining operands earns, and in which ORDER that rule is read against the
//! kinds and domains the call's signature declares for those positions.
//!
//! The rule pinned here: for every operator or function that takes a first operand and
//! remaining ones, two operands naming ONE resolved document path is refused with
//! `ValidationException` and "The first operand must be distinct from the remaining
//! operands for this operator or function; operator: <operator>, first operand: [<path
//! segments>]" — and that reading comes BEFORE the kind and the domain the signature
//! declares for the remaining positions, so a path repeated into a position that takes a
//! literal is reported as the location related to itself rather than as an operand of the
//! wrong type. The rule is stated of "this operator or function" generally (the
//! `ConditionExpression` function reference, `contains (path, operand) ... The path and the
//! operand must be distinct"), so a function whose second position takes a literal —
//! `attribute_type (path, type)` — is read for it on the same footing as one whose second
//! position takes either (`begins_with (path, substr)`).
//!
//! The rows differ along what the rule generalises over: the FUNCTION (`attribute_type`,
//! `begins_with`, `contains`), the FORM of the repeated path (a top-level name, a nested
//! path, a list element, one `#alias`, two aliases resolving to one name, an alias nested
//! under another), the expression MEMBER that carried the call (`FilterExpression`,
//! `ConditionExpression`) and the operation carrying that member (both table walks, all
//! three single-item writes, one transaction element).
//!
//! The controls hold the ORDER rather than a blanket skip: a call whose operands name two
//! DIFFERENT locations is still held to the kind its second position declares, a literal
//! outside the closed set of type codes is still named back with that set, and the same
//! calls over a path and a value the request carried are evaluated against the stored item.

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::ATTRIBUTE_TYPE_NAMES;
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row addresses: a string partition key, so a write row has a key to
/// state and a read row a table to walk.
const TABLE: &str = "FunctionOperandDistinctness";

/// The key of the one seeded item, which the write rows address.
fn key() -> Value {
    json!({"pk": {"S": "item1"}})
}

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write
/// one item, so a call the service accepts has something to be evaluated against and no
/// row is refused for its table or its key instead of for the rule under test.
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
    describe_when_active(&router, "seed the table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "item1"},
                        "info": {"M": {"label": {"S": "hello"}}},
                        "s": {"S": "beta"},
                        "tagset": {"SS": ["a", "b"]},
                        "l": {"L": [{"S": "x"}]}}}),
    )
    .await;
    router
}

/// The whole message a refusal about `member` states: the clause inside the
/// `<N> validation error(s) detected: ` envelope for the members a WRITE states its intent
/// through, and on its own for the members a READ narrows its result with — this service's
/// existing split, which an ELEMENT's own members never carry.
fn stated_for(member: &str, clause: &str) -> String {
    let clause = format!("Invalid {member}: {clause}");
    if member == "ConditionExpression" || member == "UpdateExpression" {
        return format!("1 validation error detected: {clause}");
    }
    clause
}

/// The clause a call relating one location to itself earns: the function as the expression
/// wrote it, and the repeated path as the token list AWS's refusals print a path as.
fn not_distinct(function: &str, first_operand: &str) -> String {
    format!(
        "The first operand must be distinct from the remaining operands for this operator or \
         function; operator: {function}, first operand: {first_operand}"
    )
}

/// The clause an operand of a kind its position does not take earns — the sentence the rows
/// above must NOT earn, and the one the controls below must.
fn incorrect_operand_type(function: &str, operand_type: &str) -> String {
    format!(
        "Incorrect operand type for operator or function; operator or function: {function}, \
         operand type: {operand_type}"
    )
}

/// One request that states a call, and the whole message it must earn.
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
async fn a_function_relating_one_location_to_itself_is_refused_for_non_distinctness() {
    let router = seeded().await;

    let rows = vec![
        Row {
            case: "a scan's filter testing the type of a nested path against that path",
            operation: "Scan",
            request: json!({"TableName": TABLE, "ConsistentRead": true,
                            "FilterExpression": "attribute_type(info.label, info.label)"}),
            expected: stated_for(
                "FilterExpression",
                &not_distinct("attribute_type", "[info, label]"),
            ),
        },
        Row {
            // The same call with both operands written behind aliases: distinctness is a
            // question about the locations the operands address, so the segment list is the
            // one the names resolve to.
            case: "a scan's filter testing the type of an aliased nested path against itself",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(#i.#l, #i.#l)",
                            "ExpressionAttributeNames": {"#i": "info", "#l": "label"}}),
            expected: stated_for(
                "FilterExpression",
                &not_distinct("attribute_type", "[info, label]"),
            ),
        },
        Row {
            case: "a scan's filter testing the type of a top-level attribute against itself",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(s, s)"}),
            expected: stated_for("FilterExpression", &not_distinct("attribute_type", "[s]")),
        },
        Row {
            case: "a scan's filter testing the type of a list element against itself",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(l[0], l[0])"}),
            expected: stated_for(
                "FilterExpression",
                &not_distinct("attribute_type", "[l, [0]]"),
            ),
        },
        Row {
            // Two DIFFERENT aliases for one attribute name: the text spells the operands
            // differently and they still address one location.
            case: "a scan's filter testing a type through two aliases for one name",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(#a, #b)",
                            "ExpressionAttributeNames": {"#a": "tagset", "#b": "tagset"}}),
            expected: stated_for(
                "FilterExpression",
                &not_distinct("attribute_type", "[tagset]"),
            ),
        },
        Row {
            case: "a query's filter testing the type of an attribute against itself",
            operation: "Query",
            request: json!({"TableName": TABLE, "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "item1"}},
                            "FilterExpression": "attribute_type(info, info)"}),
            expected: stated_for("FilterExpression", &not_distinct("attribute_type", "[info]")),
        },
        Row {
            // A WRITE's intent member states the same clause inside the envelope.
            case: "a put's condition testing the type of an attribute against itself",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_type(s, s)"}),
            expected: stated_for("ConditionExpression", &not_distinct("attribute_type", "[s]")),
        },
        Row {
            case: "an update's condition testing the type of a nested path against itself",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :v",
                            "ConditionExpression": "attribute_type(info.label, info.label)",
                            "ExpressionAttributeValues": {":v": {"S": "later"}}}),
            expected: stated_for(
                "ConditionExpression",
                &not_distinct("attribute_type", "[info, label]"),
            ),
        },
        Row {
            case: "a delete's condition testing the type of an aliased attribute against itself",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "attribute_type(#t, #t)",
                            "ExpressionAttributeNames": {"#t": "tagset"}}),
            expected: stated_for(
                "ConditionExpression",
                &not_distinct("attribute_type", "[tagset]"),
            ),
        },
        Row {
            // An ELEMENT's own members state the clause without the envelope, like every
            // other refusal about one element of a transaction.
            case: "a transaction element's condition testing a type against its own path",
            operation: "TransactWriteItems",
            request: json!({"TransactItems": [
                {"Update": {"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET s = :v",
                            "ConditionExpression": "attribute_type(info.label, info.label)",
                            "ExpressionAttributeValues": {":v": {"S": "later"}}}}]}),
            expected: format!(
                "Invalid ConditionExpression: {}",
                not_distinct("attribute_type", "[info, label]")
            ),
        },
        Row {
            // The two functions whose second position takes EITHER, over the same paths:
            // one rule, read the same way whatever a position would otherwise hold.
            case: "a scan's filter testing a nested path as its own prefix",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "begins_with(info.label, info.label)"}),
            expected: stated_for(
                "FilterExpression",
                &not_distinct("begins_with", "[info, label]"),
            ),
        },
        Row {
            case: "a scan's filter searching an aliased set for itself",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "contains(#t, #t)",
                            "ExpressionAttributeNames": {"#t": "tagset"}}),
            expected: stated_for("FilterExpression", &not_distinct("contains", "[tagset]")),
        },
    ];

    each_row_is_refused(&router, rows).await;
}

#[tokio::test]
async fn a_call_whose_operands_name_two_locations_is_still_held_to_its_operand_kinds() {
    let router = seeded().await;

    // The rows above are the ORDER the two rules are read in, not the kind rules going
    // unread: a call whose operands address DIFFERENT locations is held to the kind its
    // second position declares, and a literal is held to that position's domain.
    let rows = vec![
        Row {
            case: "a scan's filter testing one path's type against another path",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(info.label, s)"}),
            expected: stated_for(
                "FilterExpression",
                &incorrect_operand_type("attribute_type", "PATH"),
            ),
        },
        Row {
            case: "a put's condition testing one path's type against a nested path of it",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_type(info, info.label)"}),
            expected: stated_for(
                "ConditionExpression",
                &incorrect_operand_type("attribute_type", "PATH"),
            ),
        },
        Row {
            case: "a scan's filter testing a type against a non-string literal",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(info.label, :n)",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: stated_for(
                "FilterExpression",
                &incorrect_operand_type("attribute_type", "N"),
            ),
        },
        Row {
            // A literal that is a string but no type code: named back as the request spelled
            // it, beside the whole set of codes a stored value can carry (the members of the
            // model's `AttributeValue` union).
            case: "a scan's filter naming a type code the set does not hold",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(info.label, :t)",
                            "ExpressionAttributeValues": {":t": {"S": "STRING"}}}),
            expected: stated_for(
                "FilterExpression",
                &format!(
                    "Invalid attribute type name found; type: STRING, valid types: {{{}}}",
                    ATTRIBUTE_TYPE_NAMES.join(",")
                ),
            ),
        },
        Row {
            case: "a scan's filter giving the type test a placeholder where a path belongs",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "attribute_type(:v, :t)",
                            "ExpressionAttributeValues": {":v": {"S": "hello"},
                                                          ":t": {"S": "S"}}}),
            expected: stated_for(
                "FilterExpression",
                "Operator or function requires a document path; operator or function: \
                 attribute_type",
            ),
        },
        Row {
            case: "a scan's filter testing a numeric prefix against a path",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "FilterExpression": "begins_with(s, :n)",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            expected: stated_for(
                "FilterExpression",
                &incorrect_operand_type("begins_with", "N"),
            ),
        },
    ];

    each_row_is_refused(&router, rows).await;
}

#[tokio::test]
async fn the_same_calls_over_a_path_and_a_value_are_evaluated_against_the_item() {
    let router = seeded().await;

    // Every code the set holds, asked of the one location the rows above repeated: the type
    // test selects the item for the code that location's value carries and nothing for the
    // rest. So the refusals above are the repeated location and not a type test being
    // rejected.
    for code in ATTRIBUTE_TYPE_NAMES {
        let answered = ok(
            &router,
            "Scan",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "attribute_type(info.label, :t)",
                   "ExpressionAttributeValues": {":t": {"S": code}}}),
        )
        .await;
        let selected = if code == "S" { 1 } else { 0 };
        assert_eq!(
            answered["Count"],
            json!(selected),
            "case a type test of the stored string against the code {code:?}: expected \
             {selected} item selected, got {answered}"
        );
    }

    // And the two functions over a path and a value the request carried, each selecting the
    // seeded item by what that location holds.
    let served: Vec<(&str, Value)> = vec![
        (
            "a type test of a nested path against the code it holds",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "attribute_type(info.label, :t)",
                   "ExpressionAttributeValues": {":t": {"S": "S"}}}),
        ),
        (
            "an aliased type test of the same path",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "attribute_type(#i.#l, :t)",
                   "ExpressionAttributeNames": {"#i": "info", "#l": "label"},
                   "ExpressionAttributeValues": {":t": {"S": "S"}}}),
        ),
        (
            "a prefix test of the same path against a prefix the request carried",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "begins_with(info.label, :p)",
                   "ExpressionAttributeValues": {":p": {"S": "hel"}}}),
        ),
        (
            "a type test of a top-level map against its own code",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "attribute_type(info, :t)",
                   "ExpressionAttributeValues": {":t": {"S": "M"}}}),
        ),
        (
            "a membership test over two different locations",
            json!({"TableName": TABLE, "ConsistentRead": true,
                   "FilterExpression": "contains(tagset, :member)",
                   "ExpressionAttributeValues": {":member": {"S": "a"}}}),
        ),
    ];
    for (case, request) in &served {
        let answered = ok(&router, "Scan", request.clone()).await;
        assert_eq!(
            (&answered["Count"], &answered["Items"][0]["pk"]),
            (&json!(1), &json!({"S": "item1"})),
            "case {case:?} ({request}): expected the seeded item selected, got {answered}"
        );
    }

    // The same type test as a WRITE's condition, which the item satisfies: the write lands,
    // so the call is evaluated rather than refused.
    let updated = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": key(),
               "UpdateExpression": "SET s = :v",
               "ConditionExpression": "attribute_type(info.label, :t) AND \
                                       begins_with(info.label, :p)",
               "ExpressionAttributeValues": {":v": {"S": "later"}, ":t": {"S": "S"},
                                             ":p": {"S": "hel"}},
               "ReturnValues": "ALL_NEW"}),
    )
    .await;
    assert_eq!(
        updated["Attributes"]["s"],
        json!({"S": "later"}),
        "case a condition whose type and prefix tests hold: expected the update applied, got \
         {updated}"
    );
}
