// SPDX-License-Identifier: CC0-1.0

//! WHICH defect of a table declaration `CreateTable` reports when the request breaks a rule
//! of its KEY SCHEMA and a rule of the `AttributeDefinitions` list it draws on at the same
//! time, driven end to end over the wire.
//!
//! RULE under test: the schema the table would be created ON is settled first — every
//! element paired with the definition it keys on ("The attributes in KeySchema must also be
//! defined in the AttributeDefinitions array", `CreateTable$KeySchema`), the two key roles
//! distinct ("one KeySchemaElement (for the partition key)" and, when composite, "another
//! KeySchemaElement for the sort key"), and the elements in the order their roles require —
//! and only then the declared list's OWN defects: a name it defines twice, and a definition
//! no schema of the table uses. So a request that gets both wrong is told about the key
//! schema, which is what the table could not be built from, rather than about the
//! declaration it draws its types from.
//!
//! The rows differ along what the rule generalises over: WHICH key-schema rule the request
//! also breaks (one attribute in both key roles, an element nothing declares, an element
//! whose role is wrong for its position), whether the repeated definitions agree about the
//! TYPE, how many definitions stand beside the repeat, and the attribute names in play. The
//! CONTROLS are the same declarations with a sound key schema, which are then told about the
//! repeat — so the rows show both answers for one rule rather than only the precedence.
//!
//! Nothing a refused declaration named is created: every refused name is still absent
//! afterwards, and the one sound declaration beside them is served.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};

/// AWS's fixed sentence for a TABLE's own primary key that gives one attribute both of its
/// key roles.
const BOTH_KEY_ROLES: &str =
    "Both the Hash Key and the Range Key element in the KeySchema have the same name";

/// The opening of AWS's refusal for key attributes the request declared no definition for,
/// which carries the two lists the caller has to reconcile.
const UNDECLARED_KEYS: &str = "One or more parameter values were invalid: Some index key \
                               attributes are not defined in AttributeDefinitions. Keys: [";

/// AWS's fixed sentence for a list that defines one attribute name twice, over the name it
/// repeated.
fn duplicated(name: &str) -> String {
    format!("Attribute Name is duplicated: {name}")
}

/// One `KeySchemaElement`.
fn key(name: &str, key_type: &str) -> Value {
    json!({"AttributeName": name, "KeyType": key_type})
}

/// One `AttributeDefinitions` entry.
fn definition(name: &str, attribute_type: &str) -> Value {
    json!({"AttributeName": name, "AttributeType": attribute_type})
}

/// What the refusal of one declaration has to state.
enum Expected {
    /// That exact sentence.
    Exactly(String),
    /// A sentence opening with that text, for the refusal that echoes the request's own two
    /// lists after it.
    Opening(&'static str),
}

/// One row: the table name the declaration asks for, its key schema, its attribute
/// definitions, the global secondary indexes it declares, and the refusal it earns.
struct Row {
    case: &'static str,
    name: &'static str,
    key_schema: Vec<Value>,
    definitions: Vec<Value>,
    indexes: Vec<Value>,
    expected: Expected,
}

/// The rows: a key-schema defect beside a repeated definition, then the same declarations
/// with a sound schema.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one attribute in both key roles, declared twice as the same type",
            name: "both-roles-same-type",
            key_schema: vec![key("sku", "HASH"), key("sku", "RANGE")],
            definitions: vec![definition("sku", "S"), definition("sku", "S")],
            indexes: Vec::new(),
            expected: Expected::Exactly(BOTH_KEY_ROLES.to_string()),
        },
        Row {
            case: "one attribute in both key roles, declared twice as two types",
            name: "both-roles-two-types",
            key_schema: vec![key("sku", "HASH"), key("sku", "RANGE")],
            definitions: vec![definition("sku", "S"), definition("sku", "N")],
            indexes: Vec::new(),
            expected: Expected::Exactly(BOTH_KEY_ROLES.to_string()),
        },
        Row {
            case: "the same, under another attribute name and with a third definition beside",
            name: "both-roles-third-definition",
            key_schema: vec![key("id", "HASH"), key("id", "RANGE")],
            definitions: vec![
                definition("id", "N"),
                definition("id", "N"),
                definition("g1", "S"),
            ],
            indexes: vec![json!({
                "IndexName": "by-g1",
                "KeySchema": [key("g1", "HASH")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            })],
            expected: Expected::Exactly(BOTH_KEY_ROLES.to_string()),
        },
        Row {
            case: "a sort key nothing declares, beside a definition stated twice",
            name: "undeclared-sort-key",
            key_schema: vec![key("pk", "HASH"), key("sk", "RANGE")],
            definitions: vec![definition("pk", "S"), definition("pk", "S")],
            indexes: Vec::new(),
            expected: Expected::Opening(UNDECLARED_KEYS),
        },
        Row {
            case: "a partition key nothing declares, beside a definition stated twice",
            name: "undeclared-partition-key",
            key_schema: vec![key("missing", "HASH")],
            definitions: vec![definition("other", "S"), definition("other", "S")],
            indexes: Vec::new(),
            expected: Expected::Opening(UNDECLARED_KEYS),
        },
        Row {
            case: "a sole element in the sort-key role, beside a definition stated twice",
            name: "range-first",
            key_schema: vec![key("pk", "RANGE")],
            definitions: vec![definition("pk", "S"), definition("pk", "S")],
            indexes: Vec::new(),
            expected: Expected::Exactly(
                "Invalid KeySchema: The first KeySchemaElement is not a HASH key type".to_string(),
            ),
        },
        Row {
            case: "a sound key schema, which IS told about the repeat",
            name: "sound-schema-repeated-definition",
            key_schema: vec![key("pk", "HASH")],
            definitions: vec![definition("pk", "S"), definition("pk", "S")],
            indexes: Vec::new(),
            expected: Expected::Exactly(duplicated("pk")),
        },
        Row {
            case: "a sound composite key schema whose repeat is its sort key",
            name: "sound-composite-repeated-definition",
            key_schema: vec![key("pk", "HASH"), key("sk", "RANGE")],
            definitions: vec![
                definition("pk", "S"),
                definition("sk", "S"),
                definition("sk", "N"),
            ],
            indexes: Vec::new(),
            expected: Expected::Exactly(duplicated("sk")),
        },
        Row {
            case: "a sound schema whose repeat is an index key attribute",
            name: "sound-schema-repeated-index-definition",
            key_schema: vec![key("pk", "HASH")],
            definitions: vec![
                definition("pk", "S"),
                definition("g1", "S"),
                definition("g1", "N"),
            ],
            indexes: vec![json!({
                "IndexName": "by-g1",
                "KeySchema": [key("g1", "HASH")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            })],
            expected: Expected::Exactly(duplicated("g1")),
        },
    ]
}

/// The `CreateTable` body one row asks for.
fn create_body(row: &Row) -> Value {
    let mut body = json!({
        "TableName": row.name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": row.key_schema.clone(),
        "AttributeDefinitions": row.definitions.clone(),
    });
    if !row.indexes.is_empty() {
        body["GlobalSecondaryIndexes"] = Value::Array(row.indexes.clone());
    }
    body
}

#[tokio::test]
async fn a_declaration_breaking_both_rules_is_refused_for_its_key_schema() {
    let router = emulator();

    for row in rows() {
        let (status, refused) = call(&router, "CreateTable", create_body(&row)).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {:?}: expected a 400 ValidationException, got {status} with {refused}",
            row.case
        );
        let stated = message(&refused);
        match &row.expected {
            Expected::Exactly(sentence) => assert_eq!(
                &stated, sentence,
                "case {:?}: expected the message {sentence:?}, got {stated:?}",
                row.case
            ),
            Expected::Opening(opening) => assert!(
                stated.starts_with(opening),
                "case {:?}: expected a message opening {opening:?}, got {stated:?}",
                row.case
            ),
        }
    }
}

#[tokio::test]
async fn a_refused_declaration_creates_no_table() {
    let router = emulator();

    for row in rows() {
        call(&router, "CreateTable", create_body(&row)).await;
        let (status, described) =
            call(&router, "DescribeTable", json!({"TableName": row.name})).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {:?}: a refused create stores no table, got {status} with {described}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_same_declaration_with_both_rules_kept_is_served() {
    let router = emulator();

    // The CONTROL for every row above: one attribute per key role, each declared once, with
    // the index key attribute declared beside them.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "sound",
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [key("pk", "HASH"), key("sk", "RANGE")],
            "AttributeDefinitions": [
                definition("pk", "S"),
                definition("sk", "N"),
                definition("g1", "S"),
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": "by-g1",
                "KeySchema": [key("g1", "HASH")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
        }),
    )
    .await;
    // Read back through `DescribeTable`, which answers the declaration whatever status the
    // table is in ("You can use the DescribeTable action to check the table status"), so what
    // is asserted is the SCHEMA the create stored rather than the clock's progress on it.
    let described = ok(&router, "DescribeTable", json!({"TableName": "sound"})).await;
    let described = described["Table"].clone();
    assert_eq!(
        described["KeySchema"],
        json!([key("pk", "HASH"), key("sk", "RANGE")]),
        "the served declaration keeps the key schema it asked for, got {described}"
    );
    assert_eq!(
        described["AttributeDefinitions"],
        json!([
            definition("g1", "S"),
            definition("pk", "S"),
            definition("sk", "N"),
        ]),
        "the served declaration keeps the definitions it stated, got {described}"
    );
}
