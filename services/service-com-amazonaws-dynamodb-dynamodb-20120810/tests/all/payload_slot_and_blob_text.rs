// SPDX-License-Identifier: CC0-1.0

//! Two things a marshalling failure of this service states that a value's JSON class alone
//! does not: WHERE the unreadable value stood, and base64 text no blob can be decoded from.
//!
//! Both are the same error class as every other value the request deserializer could not read
//! — HTTP 400 under the protocol's own `SerializationException`, the sentence in the
//! capitalized `Message` and nothing in the lowercase `message` a modelled exception uses —
//! and neither reaches an operation, so nothing of such a request is looked up or applied.
//!
//! RULE 1 — a scalar standing where the model declares a type that HOLDS other values (a
//! list, a map, a structure or a union) never becomes a value, and what the refusal states
//! depends on where that scalar stood. A member a structure or a union DECLARES is a field the
//! body names, so the sentence states that the FIELD's type was unexpected; a value INSIDE a
//! collection — an element of a list, or the value of a map entry — is named by no member at
//! all, so the sentence states the value the payload carried instead. The rows span the two
//! slots, the operation (the item writes, the reads, both batches, and the legacy parameters),
//! the aggregate type declared there (list, map, structure, union), the class sent (number,
//! string, boolean) and the depth the value stands at; the controls are the conversions
//! between two single values, which name the class and the type at EITHER slot, and the
//! values that read.
//!
//! RULE 2 — a blob is carried as base64 text (`smithy_query(query="shape",
//! shape="com.amazonaws.dynamodb#AttributeValue")`: the `B` datatype targets
//! `BinaryAttributeValue`, a blob), and base64 encodes every three bytes as a group of four
//! characters, so text whose length is not a whole number of those groups decodes to no bytes
//! at all. Such text never becomes a value either, and the sentence states the one fact that
//! makes it undecodable: the length found. The rows span the length's remainder in the group
//! (1, 2, 3), the operation, and the depth the text stands at (a datatype of a value in an
//! item, an element of a binary set, a value inside a list and inside a map, a legacy update's
//! value, a batched write); the controls are texts of a whole number of groups, which decode
//! and round-trip through the store.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, ok};
use crate::table_items::create_active_table;

/// The error class a value this service could not read into the modelled input is refused
/// with, as the wire spells it.
const SERIALIZATION_EXCEPTION: &str = "com.amazon.coral.service#SerializationException";

/// What the refusal states where the scalar stood in a field the body names.
const UNEXPECTED_FIELD_TYPE: &str = "Unexpected field type";

/// What it states where the scalar stood inside a collection instead.
const UNEXPECTED_VALUE_TYPE: &str = "Unexpected value type in payload";

/// The table every row addresses: it exists and is `ACTIVE`, so nothing about these refusals
/// depends on whether the resource is there — and a refusal that never looked it up cannot
/// tell either way.
const TABLE: &str = "Slots";

/// The one assertion every row is read through: the status, the error class as the wire spells
/// it, and the sentence in the capitalized slot this class states its prose in, with the
/// lowercase slot a modelled exception uses left empty.
fn refused_as_serialization(case: &str, status: u16, body: &Value, sentence: &str) {
    let answered = body.get("__type").and_then(Value::as_str).unwrap_or_default();
    assert_eq!(
        (status, answered),
        (400, SERIALIZATION_EXCEPTION),
        "case {case:?}: expected 400 {SERIALIZATION_EXCEPTION}, got {status} with {body}"
    );
    assert_eq!(
        body.get("Message").and_then(Value::as_str),
        Some(sentence),
        "case {case:?}: expected $.Message {sentence:?}, got {body}"
    );
    assert_eq!(
        body.get("message"),
        None,
        "case {case:?}: this class states its prose under $.Message alone, got {body}"
    );
}

#[tokio::test]
async fn a_scalar_where_a_collection_is_declared_is_named_after_the_slot_it_stood_in() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, operation, the body, the sentence the refusal states)
    //
    // The modelled type of every member named here comes from `smithy_query(query="shape",
    // shape="com.amazonaws.dynamodb#AttributeValue,com.amazonaws.dynamodb#Condition,\
    // com.amazonaws.dynamodb#AttributeValueUpdate,com.amazonaws.dynamodb#WriteRequest,\
    // com.amazonaws.dynamodb#KeysAndAttributes")`: `AttributeValue` is a UNION whose `M`
    // datatype is a map and whose `L`, `SS`, `NS` and `BS` datatypes are lists, `Condition`
    // is a structure whose `AttributeValueList` is a list of `AttributeValue`,
    // `AttributeValueUpdate$Value` and the entries of an item map and of
    // `ExpressionAttributeValues` are each an `AttributeValue`, `WriteRequest` is a structure
    // whose `PutRequest` is one too, and `KeysAndAttributes$Keys` is a list of key maps.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        // A FIELD: a member the structure (or the union) declares.
        (
            "a number where the item written is a map",
            "PutItem",
            json!({"TableName": TABLE, "Item": 1}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "a flag where the key deleted is a map",
            "DeleteItem",
            json!({"TableName": TABLE, "Key": true}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "a number where a value's map datatype is a map",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "doc": {"M": 1}}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "text where a value's list datatype is a list",
            "Query",
            json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v",
                   "ExpressionAttributeValues": {":v": {"L": "x"}}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "text where the batched keys are a list",
            "BatchGetItem",
            json!({"RequestItems": {TABLE: {"Keys": "pk"}}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "a number where a batched write's put is a structure",
            "BatchWriteItem",
            json!({"RequestItems": {TABLE: [{"PutRequest": 1}]}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "a number where a legacy update's value is a value",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}},
                   "AttributeUpdates": {"n": {"Action": "PUT", "Value": 1}}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        (
            "a number where a legacy expectation's value is a value",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}},
                   "Expected": {"n": {"Value": 1, "ComparisonOperator": "EQ"}}}),
            UNEXPECTED_FIELD_TYPE,
        ),
        // INSIDE a collection: an element of a list, or the value of a map entry, neither of
        // which any member names.
        (
            "a number where an item's attribute is a value",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": 1}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "text where a legacy key condition's compared value is a value",
            "Query",
            json!({"TableName": TABLE,
                   "KeyConditions": {"pk": {"AttributeValueList": ["a"],
                                            "ComparisonOperator": "EQ"}}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "a number where an expression value is a value",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "n = :v",
                   "ExpressionAttributeValues": {":v": 1}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "a flag where a batched key is a key map's value",
            "BatchGetItem",
            json!({"RequestItems": {TABLE: {"Keys": [{"pk": true}]}}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "text where a list datatype's element is a value",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "l": {"L": ["x"]}}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "a number where a map datatype's entry is a value",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "m": {"M": {"k": 1}}}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "a number where a batched write list's element is a request",
            "BatchWriteItem",
            json!({"RequestItems": {TABLE: [1]}}),
            UNEXPECTED_VALUE_TYPE,
        ),
        (
            "a number where a transacted item is a request",
            "TransactWriteItems",
            json!({"TransactItems": [1]}),
            UNEXPECTED_VALUE_TYPE,
        ),
    ];

    for (case, operation, body, sentence) in rows {
        let (status, answered) = call(&router, operation, body).await;
        refused_as_serialization(case, status, &answered, sentence);
    }

    // The CONTROL for the slot: a single value sent where the model declares another single
    // value is a CONVERSION, which names the class sent and the type it could not become at
    // EITHER slot — so the slot decides the sentence only where a collection is involved.
    for (case, operation, body, sentence) in [
        (
            "a number where a value's string datatype is a string, in a field",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": 1}}}),
            "NUMBER_VALUE cannot be converted to String",
        ),
        (
            "a flag where a value's number datatype is a string, in a field",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"N": true}}}),
            "BOOLEAN_VALUE cannot be converted to String",
        ),
        (
            "a number where a string set's element is a string, inside the list",
            "PutItem",
            json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "s": {"SS": [1]}}}),
            "NUMBER_VALUE cannot be converted to String",
        ),
        (
            "a number where a substituted name is a string, inside the map",
            "Scan",
            json!({"TableName": TABLE, "ProjectionExpression": "#p",
                   "ExpressionAttributeNames": {"#p": 1}}),
            "NUMBER_VALUE cannot be converted to String",
        ),
    ] {
        let (status, answered) = call(&router, operation, body).await;
        refused_as_serialization(case, status, &answered, sentence);
    }

    // Nothing of a refused request was read, so nothing of it was applied: not one of the
    // writes above left an item behind.
    let scanned = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(0),
        "case \"after the refusals\": a refused write applied nothing, got {scanned}"
    );

    // And the other side of the boundary: the same members, carrying the values the model
    // declares, are served and round-trip.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "a"}, "doc": {"M": {"k": {"L": [{"N": "1"}]}}}}}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["doc"],
        json!({"M": {"k": {"L": [{"N": "1"}]}}}),
        "case \"the values that read\": the nested value is stored as sent, got {read}"
    );
}

/// The sentence text that is not a whole number of base64 groups is refused with, for the
/// length `characters` the body carried — the fixed text, which names neither the member nor
/// the text itself.
fn undecodable_length(characters: usize) -> String {
    format!("Base64 encoded length is expected a multiple of 4 bytes but found: {characters}")
}

/// Base64 alphabet characters, repeated: a text whose ONLY defect is its length, so what the
/// refusal is about is the length and nothing else about the text.
fn base64_text(characters: usize) -> String {
    "QUJD".chars().cycle().take(characters).collect()
}

/// One row of the base64 table: where the text stands and how long it is.
struct Undecodable {
    /// What the row is about, named in every assertion it drives.
    case: &'static str,
    /// The operation whose body carries the text.
    operation: &'static str,
    /// How many characters the text is — the length the refusal states.
    characters: usize,
    /// The body that carries the text at the position the row is about.
    body: fn(&str) -> Value,
}

#[tokio::test]
async fn base64_text_that_is_not_a_whole_number_of_groups_decodes_to_no_blob() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    let rows: Vec<Undecodable> = vec![
        Undecodable {
            case: "one character short of a group, written as an attribute",
            operation: "PutItem",
            characters: 3,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"B": text}}}),
        },
        Undecodable {
            case: "two characters short of a group",
            operation: "PutItem",
            characters: 2,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"B": text}}}),
        },
        Undecodable {
            case: "one character past a group",
            operation: "PutItem",
            characters: 5,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"B": text}}}),
        },
        Undecodable {
            case: "a single character",
            operation: "PutItem",
            characters: 1,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"B": text}}}),
        },
        Undecodable {
            case: "text as long as several groups and two characters over",
            operation: "PutItem",
            characters: 18,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"B": text}}}),
        },
        Undecodable {
            case: "inside a binary set",
            operation: "PutItem",
            characters: 3,
            body: |text| json!({"TableName": TABLE, "Item": {"pk": {"S": "a"}, "b": {"BS": [text]}}}),
        },
        Undecodable {
            case: "inside a list datatype",
            operation: "PutItem",
            characters: 6,
            body: |text| {
                json!({"TableName": TABLE,
                       "Item": {"pk": {"S": "a"}, "l": {"L": [{"B": text}]}}})
            },
        },
        Undecodable {
            case: "inside a map datatype",
            operation: "PutItem",
            characters: 1,
            body: |text| {
                json!({"TableName": TABLE,
                       "Item": {"pk": {"S": "a"}, "m": {"M": {"k": {"B": text}}}}})
            },
        },
        Undecodable {
            case: "in a key the read addresses",
            operation: "GetItem",
            characters: 18,
            body: |text| json!({"TableName": TABLE, "Key": {"pk": {"B": text}}}),
        },
        Undecodable {
            case: "in an expression value",
            operation: "Query",
            characters: 2,
            body: |text| {
                json!({"TableName": TABLE, "KeyConditionExpression": "pk = :v",
                       "ExpressionAttributeValues": {":v": {"B": text}}})
            },
        },
        Undecodable {
            case: "in a legacy update's value",
            operation: "UpdateItem",
            characters: 7,
            body: |text| {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "a"}},
                       "AttributeUpdates": {"b": {"Action": "PUT", "Value": {"B": text}}}})
            },
        },
        Undecodable {
            case: "in a batched write",
            operation: "BatchWriteItem",
            characters: 3,
            body: |text| {
                json!({"RequestItems":
                       {TABLE: [{"PutRequest": {"Item": {"pk": {"S": "a"},
                                                         "b": {"B": text}}}}]}})
            },
        },
    ];

    for row in rows {
        let text = base64_text(row.characters);
        let (status, answered) = call(&router, row.operation, (row.body)(&text)).await;
        refused_as_serialization(
            row.case,
            status,
            &answered,
            &undecodable_length(row.characters),
        );
    }

    // Nothing of a refused request was applied.
    let scanned = ok(&router, "Scan", json!({"TableName": TABLE})).await;
    assert_eq!(
        scanned["Count"],
        json!(0),
        "case \"after the undecodable texts\": a refused write applied nothing, got {scanned}"
    );

    // The CONTROL: text of a whole number of groups decodes, so it becomes a value, is stored
    // and is read back as the same text — on the group boundary the rows sit either side of.
    for characters in [4usize, 8, 20] {
        let text = base64_text(characters);
        let case = format!("{characters} characters, a whole number of groups");
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": text}, "b": {"B": text}, "s": {"BS": [text]}}}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": text}}, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            read["Item"]["b"],
            json!({"B": text}),
            "case {case:?}: the blob is stored as the bytes that text decodes to and read \
             back as the same text, got {read}"
        );
        assert_eq!(
            read["Item"]["s"],
            json!({"BS": [text]}),
            "case {case:?}: so is one inside a binary set, got {read}"
        );
    }
}
