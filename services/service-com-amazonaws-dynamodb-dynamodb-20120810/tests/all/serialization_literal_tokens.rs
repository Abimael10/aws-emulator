// SPDX-License-Identifier: CC0-1.0

//! What this service answers a body VALUE whose JSON form is not the one the member it is
//! bound to declares, driven end to end over the wire.
//!
//! RULE 1 — where the refusal states a conversion, the sentence is `<TOKEN> cannot be
//! converted to <Type>`, and `<TOKEN>` names the LITERAL the body carried rather than its
//! JSON type: a `true` is `TRUE_VALUE` and a `false` `FALSE_VALUE` (two literals, two
//! tokens), a number whose VALUE is a whole one is `NUMBER_VALUE` and one carrying a fraction
//! `DECIMAL_VALUE` — however the body spelled it, so a fraction written with an exponent is
//! the latter and a whole number too large for the reader's integral types the former — and
//! text is `STRING_VALUE`. `<Type>` is the Smithy shape type of the
//! member, with no article. The rows span the literal, the modelled type it could not be
//! read into (`String`, `Long`, `Integer`, `Boolean`), the operation, and the depth the
//! value stands at — a top-level member, a datatype of an `AttributeValue`, an element of a
//! set, an entry of a substitution map and a member of a nested structure
//! (`smithy_query(query="operation", operation="PutItem,Query,CreateTable,ListTables")`:
//! `TableName` targets `TableArn` (string), `Limit` an integer, `ConsistentRead` a boolean,
//! `ProvisionedThroughput$ReadCapacityUnits` a long; `smithy_query(query="shape",
//! shape="com.amazonaws.dynamodb#AttributeValue")`: the `S` and `N` datatypes are strings and
//! `SS` a list of strings).
//!
//! RULE 2 — which members the refusal BODY carries is a fact of the defect rather than of the
//! request. A JSON ARRAY standing where the member is read from no array is a value no
//! conversion was attempted for at all, so the body is the bare carrier — the `__type`
//! discriminator alone, with no prose member. An OBJECT standing there is named after the
//! structural token the reader found. A single value standing where the model declares a type
//! that HOLDS other values is named after the slot it stood in: a field the body names, or a
//! value inside a collection, which no member names. The rows span the two collections, the
//! modelled type they stood in (string, integer, boolean, map, structure), the operation and
//! the depth; nothing of any refused request is applied.
//!
//! RULE 3 — a literal the modelled type CAN be read from is read rather than refused, and the
//! value the member then carries is the one it was read as: text spelling a JSON flag is that
//! flag, and a number carrying a fraction standing where a whole number is modelled is the
//! whole number it truncates to, which is then held to the model's own range like any other.
//! The rows read the value back through what the operation DOES with it — the page a `Limit`
//! bounds, the condition an `Exists` states — rather than through the status alone, and the
//! boundary rows are the texts that spell no flag at all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, ok};

/// The error class a value this service could not read into the modelled input is refused
/// with, as the wire spells it.
const SERIALIZATION_EXCEPTION: &str = "com.amazon.coral.service#SerializationException";

/// What the refusal states where a single value stood in a FIELD the body names whose
/// modelled type holds other values.
const UNEXPECTED_FIELD_TYPE: &str = "Unexpected field type";

/// What it states where that value stood INSIDE a collection instead.
const UNEXPECTED_VALUE_TYPE: &str = "Unexpected value type in payload";

/// What it states where the body opened a structure or a map in a slot read from neither.
const STRUCTURE_OR_MAP: &str = "Start of structure or map found where not expected";

/// The table every row addresses: it exists and is `ACTIVE`, so nothing here depends on
/// whether the resource is there — and a refusal that never looked it up cannot tell either
/// way.
const TABLE: &str = "Literals";

/// The partition key of [`TABLE`].
const KEY: &str = "pk";

/// Create [`TABLE`] and wait for the `CREATING` -> `ACTIVE` transition the clock owes it.
async fn created(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, "the table the rows address", TABLE).await;
}

/// The one assertion every refusal row is read through: HTTP 400, the protocol's own error
/// class as the wire spells it, and the prose the body carries under the capitalized member —
/// `None` for the bare carrier, which carries no prose member at all and nothing besides the
/// discriminator. The lowercase `message` a MODELLED exception states its prose in is empty
/// either way.
fn refused_as_serialization(case: &str, status: u16, body: &Value, prose: Option<&str>) {
    let answered = body.get("__type").and_then(Value::as_str).unwrap_or_default();
    assert_eq!(
        (status, answered),
        (400, SERIALIZATION_EXCEPTION),
        "case {case:?}: expected 400 {SERIALIZATION_EXCEPTION}, got {status} with {body}"
    );
    assert_eq!(
        body.get("Message").and_then(Value::as_str),
        prose,
        "case {case:?}: expected $.Message {prose:?}, got {body}"
    );
    assert_eq!(
        body.get("message"),
        None,
        "case {case:?}: this class states its prose under $.Message alone, got {body}"
    );
    if prose.is_none() {
        assert_eq!(
            body.as_object().map(serde_json::Map::len),
            Some(1),
            "case {case:?}: the bare carrier carries the discriminator alone, got {body}"
        );
    }
}

#[tokio::test]
async fn a_conversion_refusal_names_the_literal_the_body_carried() {
    let router = emulator();
    created(&router).await;

    // (case, operation, the body, the sentence the refusal states)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        // The literal, where the model declares a STRING.
        (
            "a true flag where the table written to is a string",
            "PutItem",
            json!({"TableName": true, "Item": {KEY: {"S": "a"}}}),
            "TRUE_VALUE cannot be converted to String",
        ),
        (
            "a false flag where the table queried is a string",
            "Query",
            json!({"TableName": false, "KeyConditionExpression": "pk = :v"}),
            "FALSE_VALUE cannot be converted to String",
        ),
        (
            "a whole number where the table deleted from is a string",
            "DeleteItem",
            json!({"TableName": 17, "Key": {KEY: {"S": "a"}}}),
            "NUMBER_VALUE cannot be converted to String",
        ),
        (
            "a fraction where the table scanned is a string",
            "Scan",
            json!({"TableName": 17.5}),
            "DECIMAL_VALUE cannot be converted to String",
        ),
        (
            "a negative fraction where a table name is a string",
            "Scan",
            json!({"TableName": -0.25}),
            "DECIMAL_VALUE cannot be converted to String",
        ),
        (
            // A fraction is a fraction however the body spelled it.
            "an exponent-spelled fraction where a table name is a string",
            "Scan",
            json!({"TableName": 1e-7}),
            "DECIMAL_VALUE cannot be converted to String",
        ),
        (
            // And a WHOLE number is a whole one even past the width the reader holds one in,
            // where it is held as a double like a fraction.
            "a whole number past the reader's integral width, where a table name is a string",
            "Scan",
            json!({"TableName": 1e20}),
            "NUMBER_VALUE cannot be converted to String",
        ),
        // The same tokens at depth: a datatype of a value, an element of a set, an entry of
        // a substitution map.
        (
            "a true flag where a value's text datatype is a string",
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": true}}}),
            "TRUE_VALUE cannot be converted to String",
        ),
        (
            "a fraction where a value's number datatype is a string",
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": "a"}, "n": {"N": 1.5}}}),
            "DECIMAL_VALUE cannot be converted to String",
        ),
        (
            "a false flag where a string set's element is a string",
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": "a"}, "s": {"SS": [false]}}}),
            "FALSE_VALUE cannot be converted to String",
        ),
        (
            "a true flag where a substituted name is a string",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "#p",
                   "ExpressionAttributeNames": {"#p": true}}),
            "TRUE_VALUE cannot be converted to String",
        ),
        // And the modelled type the token is stated against: a long, an integer, a boolean.
        (
            "a true flag where a nested capacity is a long",
            "CreateTable",
            json!({"TableName": "Later",
                   "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
                   "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
                   "ProvisionedThroughput": {"ReadCapacityUnits": true,
                                             "WriteCapacityUnits": 5}}),
            "TRUE_VALUE cannot be converted to Long",
        ),
        (
            "a false flag where a page size is an integer",
            "ListTables",
            json!({"Limit": false}),
            "FALSE_VALUE cannot be converted to Integer",
        ),
        (
            "a whole number where a read mode is a boolean",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v",
                   "ConsistentRead": 1}),
            "NUMBER_VALUE cannot be converted to Boolean",
        ),
        (
            // Text is its own token, which is the control for the two the literals split.
            "text where a page size is an integer",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v", "Limit": "10"}),
            "STRING_VALUE cannot be converted to Integer",
        ),
    ];

    for (case, operation, body, sentence) in rows {
        let (status, answered) = call(&router, operation, body).await;
        refused_as_serialization(case, status, &answered, Some(sentence));
    }

    // Nothing of a refused request was read, so nothing of it was applied: not one of the
    // writes above left an item behind, and the create above made no table.
    let scanned = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(0),
        "case \"after the refusals\": a refused write applied nothing, got {scanned}"
    );
    let listed = ok(&router, "ListTables", json!({})).await;
    assert_eq!(
        listed["TableNames"],
        json!([TABLE]),
        "case \"after the refusals\": a refused create made no table, got {listed}"
    );
}

#[tokio::test]
async fn which_members_the_refusal_body_carries_follows_the_defect() {
    let router = emulator();
    created(&router).await;

    // (case, operation, the body, the prose the refusal states — `None` for the bare carrier)
    let rows: Vec<(&str, &str, Value, Option<&str>)> = vec![
        // An ARRAY where the member is read from none: no conversion was attempted, so the
        // body carries the error class alone.
        (
            "a list where the substituted values are a map",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v",
                   "ExpressionAttributeValues": []}),
            None,
        ),
        (
            "a list where a table name is a string",
            "PutItem",
            json!({"TableName": [TABLE], "Item": {KEY: {"S": "a"}}}),
            None,
        ),
        (
            "a list where the batched tables are a map",
            "BatchGetItem",
            json!({"RequestItems": []}),
            None,
        ),
        (
            "a list where the legacy expectations are a map",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {KEY: {"S": "a"}}, "Expected": []}),
            None,
        ),
        (
            "a list where a page size is an integer",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v", "Limit": []}),
            None,
        ),
        (
            "a list where a read mode is a boolean",
            "Scan",
            json!({"TableName": TABLE, "ConsistentRead": [1]}),
            None,
        ),
        (
            "a list where a value's text datatype is a string, inside the item map",
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": ["a"]}}}),
            None,
        ),
        // An OBJECT there: the structural sentence, at either slot.
        (
            "an object where a table name is a string",
            "DeleteItem",
            json!({"TableName": {"Name": TABLE}, "Key": {KEY: {"S": "a"}}}),
            Some(STRUCTURE_OR_MAP),
        ),
        (
            "an object where a page size is an integer",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v", "Limit": {}}),
            Some(STRUCTURE_OR_MAP),
        ),
        (
            "an object where a read mode is a boolean",
            "Scan",
            json!({"TableName": TABLE, "ConsistentRead": {}}),
            Some(STRUCTURE_OR_MAP),
        ),
        (
            "an object where a value's text datatype is a string, inside the item map",
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": {"text": "a"}}}}),
            Some(STRUCTURE_OR_MAP),
        ),
        // A SINGLE value where the model declares a type that holds other values: named
        // after the slot it stood in.
        (
            "a number where the item written is a map",
            "PutItem",
            json!({"TableName": TABLE, "Item": 1}),
            Some(UNEXPECTED_FIELD_TYPE),
        ),
        (
            "a flag where the substituted values are a map",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v",
                   "ExpressionAttributeValues": true}),
            Some(UNEXPECTED_FIELD_TYPE),
        ),
        (
            "a number where an expression value is a value",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "n = :v",
                   "ExpressionAttributeValues": {":v": 1}}),
            Some(UNEXPECTED_VALUE_TYPE),
        ),
    ];

    for (case, operation, body, prose) in rows {
        let (status, answered) = call(&router, operation, body).await;
        refused_as_serialization(case, status, &answered, prose);
    }

    // Nothing of a refused request was applied.
    let scanned = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(0),
        "case \"after the refusals\": a refused write applied nothing, got {scanned}"
    );
}

#[tokio::test]
async fn a_literal_the_member_can_be_read_from_is_read_rather_than_refused() {
    let router = emulator();
    created(&router).await;
    for key in ["a", "b", "c"] {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": key}, "attr": {"N": "1"}}}),
        )
        .await;
    }

    // A fraction where the page size is a whole number: the page is the one the truncated
    // number asks for, which is what says the value was READ rather than ignored. (The codec
    // itself reads a number whose fraction is zero, so the reading here is that same
    // narrowing carried one step further.)
    for (case, limit, expected) in [
        ("a page size of 2.7", json!(2.7), 2),
        ("a page size of 1.2", json!(1.2), 1),
        ("a page size of 2.0", json!(2.0), 2),
    ] {
        let scanned = ok(
            &router,
            "Scan",
            json!({"TableName": TABLE, "Limit": limit}),
        )
        .await;
        assert_eq!(
            scanned["Count"],
            json!(expected),
            "case {case:?}: the page is bounded by the whole number the literal is read as, \
             got {scanned}"
        );
        assert!(
            scanned.get("LastEvaluatedKey").is_some(),
            "case {case:?}: a bounded page that stopped short reports where it stopped, got \
             {scanned}"
        );
    }

    // The value read is then held to the model's own range like any other, which is the one
    // observable difference between truncating towards zero and away from it.
    let (status, answered) = call(
        &router,
        "Scan",
        json!({"TableName": TABLE, "Limit": 0.5}),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ValidationException"),
        "case \"a page size of 0.5\": the whole number it is read as is below the modelled \
         range, got {status} with {answered}"
    );

    // Text spelling a JSON flag is read as that flag, wherever the model declares a boolean:
    // the read mode of a read, and the existence a legacy expectation states.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY: {"S": "a"}}, "ConsistentRead": "true"}),
    )
    .await;
    assert_eq!(
        read["Item"][KEY],
        json!({"S": "a"}),
        "case \"a read mode spelled as text\": the read is served and answers the item, got \
         {read}"
    );
    let (status, answered) = call(
        &router,
        "DeleteItem",
        json!({"TableName": TABLE, "Key": {KEY: {"S": "a"}},
               "Expected": {"attr": {"Exists": "false"}}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ConditionalCheckFailedException"),
        "case \"an expectation spelled as text\": the flag the text spells is the condition \
         evaluated, and the item does carry the attribute, got {status} with {answered}"
    );
    let kept = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY: {"S": "a"}}}),
    )
    .await;
    assert_eq!(
        kept["Item"]["attr"],
        json!({"N": "1"}),
        "case \"an expectation spelled as text\": the refused delete left the item, got {kept}"
    );
    let deleted = ok(
        &router,
        "DeleteItem",
        json!({"TableName": TABLE, "Key": {KEY: {"S": "b"}},
               "Expected": {"attr": {"Exists": "true", "Value": {"N": "1"}}}}),
    )
    .await;
    assert!(
        deleted.get("__type").is_none(),
        "case \"an expectation whose text spells true\": the condition holds, so the delete \
         lands, got {deleted}"
    );

    // The BOUNDARY: text that spells no JSON flag is a value no boolean can be read from, so
    // it is refused as the marshalling failure it is.
    for (case, sent) in [
        ("text spelling no flag at all", "yes"),
        ("the flag spelled with a capital", "True"),
        ("empty text", ""),
    ] {
        let (status, answered) = call(
            &router,
            "Scan",
            json!({"TableName": TABLE, "ConsistentRead": sent}),
        )
        .await;
        refused_as_serialization(
            case,
            status,
            &answered,
            Some("STRING_VALUE cannot be converted to Boolean"),
        );
    }

    // And what a body states is a fact of the body as the protocol READS it: an explicit
    // `null` standing for a member the input REQUIRES earns the reader's own diagnosis of it
    // — in the lowercase member that diagnosis is stated in — whether or not a value the
    // protocol reads in another form stands before it in the body.
    for (case, body) in [
        (
            "a null required member on its own",
            json!({"TableName": null, "Key": {KEY: {"S": "a"}}}),
        ),
        (
            "a null required member behind a flag spelled as text",
            json!({"TableName": null, "Key": {KEY: {"S": "a"}},
                   "ConsistentRead": "true"}),
        ),
    ] {
        let (status, answered) = call(&router, "GetItem", body).await;
        let diagnosed = answered
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            (
                status,
                answered.get("__type").and_then(Value::as_str),
                diagnosed.contains("expected a string"),
            ),
            (400, Some(SERIALIZATION_EXCEPTION), true),
            "case {case:?}: expected the reader's own diagnosis of the absence, got {status} \
             with {answered}"
        );
    }
}
