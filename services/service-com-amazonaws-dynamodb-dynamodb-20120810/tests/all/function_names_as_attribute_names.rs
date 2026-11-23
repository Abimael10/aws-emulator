// SPDX-License-Identifier: CC0-1.0

//! What an expression does with a bare word that spells one of the expression language's
//! own FUNCTIONS where the grammar wants a name.
//!
//! One rule, over every surface that reads a name out of an expression: a word the
//! expression language spells as one of its functions names that function, so where the
//! grammar wants a document path the word reads as the ATTRIBUTE of that name and the
//! request is served — it is not held to DynamoDB's reserved-word list, which is what
//! refuses a bare word that names no function of the language.
//!
//! The rows differ along what the rule generalises over: WHICH function name is written
//! (every one the condition and update references define, so the table is the language's
//! function set rather than the one word a recording happened to carry), the member the
//! expression travelled in (`ProjectionExpression`, `FilterExpression`,
//! `ConditionExpression`, `UpdateExpression`, `KeyConditionExpression`), the operation
//! that carried it, and the position the word stands in (a projected path, a comparison's
//! first operand, a function argument, an update target, a key condition's key).
//!
//! Each half carries the other as its CONTROL: a reserved word that names no function of
//! the language is still refused, in that member's own words, whatever its case and
//! wherever in the path it stands, and the same name reached through
//! `ExpressionAttributeNames` is served — which is what that member is for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table the rows below address, keyed on one string attribute.
const TABLE: &str = "FunctionNameAttributes";

/// A second table whose PARTITION KEY is named like a function of the language, so a
/// `KeyConditionExpression` can write that name where a key belongs.
const KEYED_TABLE: &str = "FunctionNameKeyed";

/// The FUNCTIONS the expression language defines, with the value this suite stores under
/// each as an attribute name.
///
/// The set is the language's own, read off the documented signatures the
/// `ConditionExpression` and `UpdateExpression` function references state —
/// `attribute_exists (path)`, `attribute_not_exists (path)`, `attribute_type (path, type)`,
/// `begins_with (path, substr)`, `contains (path, operand)`, `size (path)`,
/// `if_not_exists (path, operand)`, `list_append (operand, operand)` — rather than the one
/// name the issue's recorded read projected.
fn function_name_attributes() -> Vec<(&'static str, Value)> {
    vec![
        ("size", json!({"N": "7"})),
        ("attribute_exists", json!({"BOOL": true})),
        ("attribute_not_exists", json!({"BOOL": false})),
        ("attribute_type", json!({"S": "S"})),
        ("begins_with", json!({"S": "prefix"})),
        ("contains", json!({"S": "member"})),
        ("if_not_exists", json!({"S": "fallback"})),
        ("list_append", json!({"L": [{"S": "tail"}]})),
    ]
}

/// The item this suite reads: one attribute per function name of the language, beside
/// reserved words that name no function of it and a map holding one of those reserved
/// words as a nested member.
fn seed_item() -> Value {
    let mut item = json!({"pk": {"S": "one"},
                          "name": {"S": "n"},
                          "status": {"S": "live"},
                          "zone": {"S": "z"},
                          "m": {"M": {"name": {"S": "inner"}}}});
    for (attribute, value) in function_name_attributes() {
        item[attribute] = value;
    }
    item
}

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and store
/// [`seed_item`], so a projection the service accepts has an item to report.
async fn seeded(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, "seed the table", TABLE).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE, "Item": seed_item()}),
    )
    .await;
}

/// The whole message a refusal about `member`'s expression carries: the clause stands on
/// its own for the members a READ narrows its result with, and inside the
/// `<N> validation error(s) detected: ` envelope for the members a WRITE states its intent
/// through.
fn stated_for(member: &str, clause: &str) -> String {
    let clause = format!("Invalid {member}: {clause}");
    if member == "ConditionExpression" || member == "UpdateExpression" {
        return format!("1 validation error detected: {clause}");
    }
    clause
}

/// The clause a bare reserved word written as an attribute name earns, which echoes the
/// word AS THE REQUEST SPELLED IT.
fn reserved(word: &str) -> String {
    format!("Attribute name is a reserved keyword; reserved keyword: {word}")
}

#[tokio::test]
async fn a_projection_reports_the_attribute_a_function_name_names() {
    let router = emulator();
    seeded(&router).await;

    // One row per function name the language defines: the projection names `pk` and that
    // attribute, and the read reports exactly those two.
    for (attribute, value) in function_name_attributes() {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": format!("pk, {attribute}")}),
        )
        .await;
        let expected = json!({"pk": {"S": "one"}, attribute: value});
        assert_eq!(
            read["Item"], expected,
            "case project {attribute:?}: expected the item to report {expected}, got {read}"
        );
    }

    // The CONTROL: a bare word that names no function of the language is a reserved word,
    // and the projection is refused for it — whatever its case, and wherever in the
    // document path it stands.
    let refused: Vec<(&str, Value, String)> = vec![
        (
            "a reserved word as the whole path",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "pk, name"}),
            reserved("name"),
        ),
        (
            // The reserved-word list is matched without regard to case, and the refusal
            // echoes the spelling the request sent.
            "a reserved word spelled in mixed case",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "pk, NaMe"}),
            reserved("NaMe"),
        ),
        (
            // `ZONE` is the last entry of the published reserved-word list, which the
            // issue's own example is nowhere near.
            "a reserved word from the end of the list",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "pk, zone"}),
            reserved("zone"),
        ),
        (
            "a reserved word inside a document path",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "m.name"}),
            reserved("name"),
        ),
        (
            // "These function names are case-sensitive" (the `ConditionExpression`
            // function reference), so a word that only differs from a function name in
            // case names no function of the language: it is an ordinary word, and the
            // reserved-word list — matched without regard to case — refuses it.
            "a function name spelled in another case",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "ProjectionExpression": "pk, SIZE"}),
            reserved("SIZE"),
        ),
    ];
    for (case, request, clause) in &refused {
        let (status, body) = call(&router, "GetItem", request.clone()).await;
        let expected = stated_for("ProjectionExpression", clause);
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: expected the refusal {expected:?}, got {status} with {body}"
        );
    }

    // The other CONTROL: the reserved name reached through `ExpressionAttributeNames` is
    // served — which is what that member is for.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "ProjectionExpression": "pk, #n",
               "ExpressionAttributeNames": {"#n": "name"}}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"pk": {"S": "one"}, "name": {"S": "n"}}),
        "case project a reserved name through an alias: expected the item to report it, got \
         {read}"
    );
}

#[tokio::test]
async fn every_expression_member_reads_a_function_name_as_an_attribute() {
    let router = emulator();
    seeded(&router).await;
    // The second table is keyed ON a function name, which is the only way a key condition
    // can write one where a key belongs.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": KEYED_TABLE,
            "KeySchema": [{"AttributeName": "size", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "size", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "seed the keyed table", KEYED_TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": KEYED_TABLE, "Item": {"size": {"S": "k"}, "held": {"S": "v"}}}),
    )
    .await;

    // A FILTER reads the attribute of that name: the stored item matches the value it
    // holds, and no item matches another value, so the word addressed the attribute rather
    // than being read as a call to the function.
    let matched = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "size = :held",
               "ExpressionAttributeValues": {":held": {"N": "7"}}}),
    )
    .await;
    assert_eq!(
        matched["Count"], json!(1),
        "case filter on the attribute a function name names: expected the stored item, got \
         {matched}"
    );
    let unmatched = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE, "FilterExpression": "size = :other",
               "ExpressionAttributeValues": {":other": {"N": "8"}}}),
    )
    .await;
    assert_eq!(
        unmatched["Count"], json!(0),
        "case filter on another value of that attribute: expected no item, got {unmatched}"
    );

    // A CONDITION reads it too: the stored item HAS the attribute, so a write gated on its
    // absence is refused as the conditional check it is — not as a malformed expression.
    let (status, body) = call(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "one"}},
               "ConditionExpression": "attribute_not_exists(size)"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ConditionalCheckFailedException"),
        "case gate a write on that attribute's absence: expected the conditional refusal, got \
         {status} with {body}"
    );
    // And the same condition over a key that holds no item at all is met, so the write
    // lands.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "two"}},
               "ConditionExpression": "attribute_not_exists(size)"}),
    )
    .await;

    // An UPDATE targets it, and the round trip through `GetItem` reports what the update
    // stored.
    ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "UpdateExpression": "SET size = :grown",
               "ExpressionAttributeValues": {":grown": {"N": "11"}}}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
               "ProjectionExpression": "size"}),
    )
    .await;
    assert_eq!(
        read["Item"],
        json!({"size": {"N": "11"}}),
        "case update the attribute a function name names: expected the stored value, got {read}"
    );

    // A KEY CONDITION writes it where a key belongs, and the query answers the item held
    // under that key.
    let queried = ok(
        &router,
        "Query",
        json!({"TableName": KEYED_TABLE, "KeyConditionExpression": "size = :k",
               "ExpressionAttributeValues": {":k": {"S": "k"}}}),
    )
    .await;
    assert_eq!(
        queried["Items"],
        json!([{"size": {"S": "k"}, "held": {"S": "v"}}]),
        "case query a table keyed on that name: expected the stored item, got {queried}"
    );

    // The CONTROL in each of those members: a bare word that names no function of the
    // language is still refused as the reserved word it is, in that member's own words.
    let refused: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "a reserved word in a filter",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "name = :v",
                   "ExpressionAttributeValues": {":v": {"S": "n"}}}),
            "FilterExpression",
            reserved("name"),
        ),
        (
            "a reserved word in a condition",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "three"}},
                   "ConditionExpression": "attribute_not_exists(status)"}),
            "ConditionExpression",
            reserved("status"),
        ),
        (
            "a reserved word as an update target",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}},
                   "UpdateExpression": "SET zone = :v",
                   "ExpressionAttributeValues": {":v": {"S": "z"}}}),
            "UpdateExpression",
            reserved("zone"),
        ),
        (
            "a reserved word in a key condition",
            "Query",
            json!({"TableName": KEYED_TABLE, "KeyConditionExpression": "name = :v",
                   "ExpressionAttributeValues": {":v": {"S": "n"}}}),
            "KeyConditionExpression",
            reserved("name"),
        ),
    ];
    for (case, operation, request, member, clause) in &refused {
        let (status, body) = call(&router, operation, request.clone()).await;
        let expected = stated_for(member, clause);
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", expected.as_str()),
            "case {case:?} ({operation}): expected the refusal {expected:?}, got {status} with \
             {body}"
        );
    }
}
