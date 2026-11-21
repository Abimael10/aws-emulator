// SPDX-License-Identifier: CC0-1.0

//! What a TABLE DECLARATION this service refuses states, and in which ORDER it states the
//! defects of a declaration that breaks several rules at once — driven end to end over the
//! wire.
//!
//! Four rules, each over a table of inputs:
//!
//! RULE 1 — an `AttributeDefinitions` list that defines one attribute NAME twice states two
//! declarations for one attribute of the item, so there is no single type a key could be
//! built on: the call is refused with AWS's fixed sentence over the repeated name, and
//! nothing is created or changed. The types the two entries state do not enter it, and the
//! rule holds wherever that list is carried — the create's own, and the update that adds a
//! secondary index.
//!
//! RULE 2 — a `StreamSpecification` that turns a stream ON without naming a view type to
//! capture in is refused with the sentence AWS words for a table being CREATED, which is the
//! mirror image of the one it words for a create that names a view type with the stream off.
//!
//! RULE 3 — a declaration that is both missing the capacity its billing mode requires AND
//! malformed in its key schema is told about the CAPACITY first: the two are independent
//! declarations, and a caller repairing the request follows the capacity defect before the
//! schema one. The control is the same key schema with the capacity stated, which is then
//! told about the schema.
//!
//! RULE 4 — where ONE value breaks a bound in two lists at once, the clauses are stated in
//! the order this service states those two members: an over-long attribute name is reported
//! under the KEY SCHEMA before the attribute definitions, counted in one envelope.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table rule 1's update rows address.
const TABLE: &str = "Declared";

/// AWS's fixed sentence for an attribute defined twice, over the repeated name.
fn duplicated(name: &str) -> String {
    format!("Attribute Name is duplicated: {name}")
}

/// AWS's fixed sentence for a create that enables a stream and names no view type.
const STREAM_WITHOUT_VIEW_TYPE: &str =
    "One or more parameter values were invalid: Table is being created with a stream enabled, \
     UpdateViewType is required";

/// AWS's fixed sentence for a create that leaves the stream off and names a view type anyway
/// — the other half of the same pair, which is what makes the sentence above the one worded
/// for a CREATE rather than a general one.
const STREAM_VIEW_TYPE_WITHOUT_A_STREAM: &str =
    "One or more parameter values were invalid: Table is being created with a stream disabled, \
     UpdateViewType should not be specified";

/// AWS's fixed sentence for a `PROVISIONED` declaration that states no capacity.
const CAPACITY_REQUIRED: &str =
    "One or more parameter values were invalid: ReadCapacityUnits and WriteCapacityUnits must \
     both be specified when BillingMode is PROVISIONED";

/// A `CreateTable` body with `extra`'s members merged into a declaration that is otherwise
/// complete: one `pk` partition key, declared as a string, on an on-demand table.
fn create_body(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    if let Some(members) = extra.as_object() {
        for (member, held) in members {
            body[member] = held.clone();
        }
    }
    body
}

#[tokio::test]
async fn an_attribute_defined_twice_is_refused_wherever_the_list_is_carried() {
    let router = emulator();
    ok(&router, "CreateTable", create_body(TABLE, json!({}))).await;
    describe_when_active(&router, "the table the updates address", TABLE).await;

    // (case, operation, request, the repeated name)
    //
    // The rows differ along the operation carrying the list, whether the two entries agree
    // about the TYPE, how many other definitions stand beside them, and which position the
    // repeat stands at.
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a create defining one name as two types",
            "CreateTable",
            create_body(
                "dup-types",
                json!({"AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                                {"AttributeName": "pk", "AttributeType": "N"}]}),
            ),
            "pk",
        ),
        (
            "a create defining one name twice as the same type",
            "CreateTable",
            create_body(
                "dup-same-type",
                json!({"AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                                {"AttributeName": "pk", "AttributeType": "S"}]}),
            ),
            "pk",
        ),
        (
            "a create whose repeat stands behind another definition",
            "CreateTable",
            create_body(
                "dup-later",
                json!({
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                                  {"AttributeName": "sk", "KeyType": "RANGE"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                             {"AttributeName": "sk", "AttributeType": "S"},
                                             {"AttributeName": "sk", "AttributeType": "N"}],
                }),
            ),
            "sk",
        ),
        (
            "an update adding an index that defines one name twice",
            "UpdateTable",
            json!({
                "TableName": TABLE,
                "AttributeDefinitions": [{"AttributeName": "g1", "AttributeType": "N"},
                                         {"AttributeName": "g1", "AttributeType": "S"}],
                "GlobalSecondaryIndexUpdates": [{"Create": {
                    "IndexName": "by-g1",
                    "KeySchema": [{"AttributeName": "g1", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                }}],
            }),
            "g1",
        ),
    ];

    for (case, operation, request, name) in rows {
        let (status, refused) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?} ({operation}): expected a 400 ValidationException, got {status} with \
             {refused}"
        );
        let expected = duplicated(name);
        let stated = message(&refused);
        assert_eq!(
            stated, expected,
            "case {case:?} ({operation}): expected the message {expected:?}, got {stated:?}"
        );
    }

    // Nothing of a refused declaration was stored: neither created table exists, and the
    // table the update addressed is settled, carries no index and kept its definitions.
    for name in ["dup-types", "dup-same-type", "dup-later"] {
        let (status, described) = call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "a refused create stores no table, got {status} with {described}"
        );
    }
    let described = describe_when_active(&router, "after the refused update", TABLE).await;
    assert!(
        described.get("GlobalSecondaryIndexes").is_none(),
        "a refused update added no index, got {described}"
    );
    assert_eq!(
        described["AttributeDefinitions"],
        json!([{"AttributeName": "pk", "AttributeType": "S"}]),
        "a refused update changed no definition, got {described}"
    );

    // The CONTROL: the same update with each name defined once is served, and the index it
    // adds is reported on the table.
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{"AttributeName": "g1", "AttributeType": "N"}],
            "GlobalSecondaryIndexUpdates": [{"Create": {
                "IndexName": "by-g1",
                "KeySchema": [{"AttributeName": "g1", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }}],
        }),
    )
    .await;
    let updated = describe_when_active(&router, "after the accepted update", TABLE).await;
    assert_eq!(
        updated["GlobalSecondaryIndexes"][0]["IndexName"],
        json!("by-g1"),
        "the accepted update added the index it named, got {updated}"
    );
}

#[tokio::test]
async fn a_stream_enabled_without_a_view_type_is_refused_in_the_words_a_create_states() {
    let router = emulator();

    // (case, the specification the create carries, the sentence it earns)
    //
    // The rows are the two halves of one rule: a stream ON with no view type, and a view
    // type named with the stream OFF.
    for (case, specification, expected) in [
        (
            "a stream enabled with no view type",
            json!({"StreamEnabled": true}),
            STREAM_WITHOUT_VIEW_TYPE,
        ),
        (
            "a view type named with the stream disabled",
            json!({"StreamEnabled": false, "StreamViewType": "NEW_IMAGE"}),
            STREAM_VIEW_TYPE_WITHOUT_A_STREAM,
        ),
    ] {
        let name = format!("streamed-{}", case.replace(' ', "-"));
        let (status, refused) = call(
            &router,
            "CreateTable",
            create_body(&name, json!({"StreamSpecification": specification})),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {refused}"
        );
        let stated = message(&refused);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the message {expected:?}, got {stated:?}"
        );
        let (described_status, described) =
            call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (described_status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: a refused create stores no table, got {described_status} with \
             {described}"
        );
    }

    // The CONTROL: the specification with both members stated is served, and the table
    // reports the stream it asked for.
    ok(
        &router,
        "CreateTable",
        create_body(
            "streamed",
            json!({"StreamSpecification": {"StreamEnabled": true,
                                           "StreamViewType": "NEW_IMAGE"}}),
        ),
    )
    .await;
    let described = describe_when_active(&router, "a stream the create states whole", "streamed")
        .await;
    assert_eq!(
        described["StreamSpecification"],
        json!({"StreamEnabled": true, "StreamViewType": "NEW_IMAGE"}),
        "the table reports the stream the create asked for, got {described}"
    );
}

#[tokio::test]
async fn a_declaration_missing_its_capacity_is_told_about_that_before_its_key_schema() {
    let router = emulator();

    // (case, the key schema and definitions the declaration carries)
    //
    // Each row is a PROVISIONED declaration (the mode a request that names none defaults to)
    // with no capacity AND a key schema that breaks a rule of its own; the rows differ along
    // which schema rule is broken, so the precedence is not pinned to one companion defect.
    // Every schema here is one the declaration's own walk refuses rather than one the model's
    // declared bounds do: a bound the MODEL declares (an empty list, past the length its
    // member admits) is reported by the validating walk before any handler runs, which is a
    // different layer and a different rule.
    let malformed: Vec<(&str, Value)> = vec![
        (
            "a second element that is not a sort key",
            json!({"KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                                 {"AttributeName": "sk", "KeyType": "HASH"}],
                   "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                            {"AttributeName": "sk", "AttributeType": "S"}]}),
        ),
        (
            "a key attribute nothing declares",
            json!({"KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                   "AttributeDefinitions": [{"AttributeName": "other", "AttributeType": "S"}]}),
        ),
        (
            "one attribute in both key positions",
            json!({"KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                                 {"AttributeName": "pk", "KeyType": "RANGE"}],
                   "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                            {"AttributeName": "sk", "AttributeType": "S"}]}),
        ),
        (
            "a first element that is not a partition key",
            json!({"KeySchema": [{"AttributeName": "pk", "KeyType": "RANGE"}],
                   "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}]}),
        ),
    ];

    for (case, mut declaration) in malformed {
        let name = format!("ordered-{}", case.replace(' ', "-"));
        declaration["TableName"] = json!(name);
        declaration["BillingMode"] = json!("PROVISIONED");
        let (status, refused) = call(&router, "CreateTable", declaration.clone()).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {refused}"
        );
        let stated = message(&refused);
        assert_eq!(
            stated, CAPACITY_REQUIRED,
            "case {case:?}: the capacity defect is reported first, expected \
             {CAPACITY_REQUIRED:?}, got {stated:?}"
        );

        // The CONTROL: the same declaration with the capacity stated is told about the key
        // schema instead, so the row's schema really is malformed.
        declaration["ProvisionedThroughput"] =
            json!({"ReadCapacityUnits": 1, "WriteCapacityUnits": 1});
        let (schema_status, schema_refused) =
            call(&router, "CreateTable", declaration.clone()).await;
        let schema_stated = message(&schema_refused);
        assert_eq!(
            (schema_status, error_code(&schema_refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the schema defect to be reported, got {schema_status} \
             with {schema_refused}"
        );
        assert_ne!(
            schema_stated, CAPACITY_REQUIRED,
            "case {case:?}: with the capacity stated the refusal is about the schema, got \
             {schema_stated:?}"
        );
        let (described_status, described) =
            call(&router, "DescribeTable", json!({"TableName": name})).await;
        assert_eq!(
            (described_status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: neither refusal stored a table, got {described_status} with \
             {described}"
        );
    }

    // And the sentence the first row's schema earns, in full: the element at fault and the
    // key type its position required.
    let (status, refused) = call(
        &router,
        "CreateTable",
        json!({"TableName": "ordered-second-element",
               "BillingMode": "PROVISIONED",
               "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
               "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                             {"AttributeName": "sk", "KeyType": "HASH"}],
               "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                        {"AttributeName": "sk", "AttributeType": "S"}]}),
    )
    .await;
    assert_eq!(
        (status, message(&refused).as_str()),
        (
            400,
            "Invalid KeySchema: The second KeySchemaElement is not a RANGE key type"
        ),
        "the schema defect names the element and the key type its position required, got \
         {status} with {refused}"
    );
}

#[tokio::test]
async fn one_over_long_attribute_name_is_reported_under_the_key_schema_first() {
    let router = emulator();

    // `AttributeName` is bounded at 255 characters in both lists
    // (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#KeySchemaElement")` and
    // `shape="com.amazonaws.dynamodb#AttributeDefinition"`), so one name past it breaks a
    // bound in each. The rows are the first length past the bound and one far past it; the
    // whole value is echoed in each clause, which is why the longer row stays inside what a
    // clause quotes.
    for (case, length) in [("one character past the bound", 256), ("far past it", 300)] {
        let name = "k".repeat(length);
        let (status, refused) = call(
            &router,
            "CreateTable",
            json!({"TableName": format!("named-{length}"),
                   "BillingMode": "PAY_PER_REQUEST",
                   "KeySchema": [{"AttributeName": name, "KeyType": "HASH"}],
                   "AttributeDefinitions": [{"AttributeName": name, "AttributeType": "S"}]}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} with {refused}"
        );
        let stated = message(&refused);
        let key_schema = stated
            .find("at 'keySchema.1.member.attributeName'")
            .unwrap_or(usize::MAX);
        let definitions = stated
            .find("at 'attributeDefinitions.1.member.attributeName'")
            .unwrap_or(usize::MAX);
        assert!(
            stated.starts_with("2 validation errors detected: "),
            "case {case:?}: one name breaking a bound in both lists is two clauses in one \
             envelope, got {stated:?}"
        );
        assert!(
            key_schema < definitions && definitions != usize::MAX,
            "case {case:?}: the key schema's clause is stated first, got {stated:?}"
        );
    }

    // And the whole message for the first length past the bound, which is the one a clause
    // quotes the value of in full.
    let name = "k".repeat(256);
    let (_, refused) = call(
        &router,
        "CreateTable",
        json!({"TableName": "named-exactly",
               "BillingMode": "PAY_PER_REQUEST",
               "KeySchema": [{"AttributeName": name, "KeyType": "HASH"}],
               "AttributeDefinitions": [{"AttributeName": name, "AttributeType": "S"}]}),
    )
    .await;
    let expected = format!(
        "2 validation errors detected: Value '{name}' at 'keySchema.1.member.attributeName' \
         failed to satisfy constraint: Member must have length less than or equal to 255; \
         Value '{name}' at 'attributeDefinitions.1.member.attributeName' failed to satisfy \
         constraint: Member must have length less than or equal to 255"
    );
    let stated = message(&refused);
    assert_eq!(
        stated, expected,
        "expected the message {expected:?}, got {stated:?}"
    );
}
