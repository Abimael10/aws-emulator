// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a `CreateTable` declaration's own `KeySchema` is refused.
//!
//! The rules pinned here:
//!
//! 1. a table's primary key is "one `KeySchemaElement` (for the partition key)" and, when
//!    composite, "another `KeySchemaElement` for the sort key" (the `KeySchemaElement`
//!    doc), so a longer list is outside the length that member admits — which is a
//!    CONSTRAINT on the member and is refused the way the request validator refuses one:
//!    the counted envelope, the member's lowerCamel path, the submitted list echoed in the
//!    object rendering a refused structure value carries, and the length requirement. The
//!    clause states no count of its own and no emulator figure;
//! 2. a key element is resolved to the attribute it keys on BEFORE the role that element
//!    plays is read: a schema naming an attribute `AttributeDefinitions` does not declare
//!    is refused for the missing definition whatever its key types say, and whatever else
//!    the declaration carries. WHICH sentence states that follows from the COUNT the
//!    request declared: fewer definitions than the schema has elements cannot be one per
//!    element, so the fact is the element left with no definition, in a sentence that
//!    interpolates nothing; at least as many definitions means the request stated enough
//!    of them and got the NAMES wrong, so it is told both lists — the whole key schema
//!    beside every declared attribute name — and a definition no key schema uses does not
//!    change that. A schema whose elements are all declared is then held to the roles,
//!    which is the refusal that names the element;
//! 3. an INDEX key schema of the kind that admits SEVERAL keys of a role — a global
//!    secondary index, with "up to 4 partition keys and up to 4 sort keys"
//!    (`CreateTable$GlobalSecondaryIndexes`) — that carries more of a role than it admits
//!    has broken that role's COUNT rather than put one element out of place, so the refusal
//!    states the count's own fixed sentence naming that role and names neither an element
//!    nor the index. A schema whose kind admits ONE key of each role keeps the refusal that
//!    names the element by its ordinal, which is how the table's own primary key is
//!    refused;
//! 4. two clauses of one refusal are stated in the order the service states those members:
//!    the capacity MODE's before the key schema's;
//! 5. a refused declaration stores no table.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and each rule is asserted over a
//! SET of inputs — how many elements the schema carries, how they are spelt, which roles
//! they claim, which element has no definition and what else the request declares — rather
//! than one example.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// One key schema as a row writes it: each element's attribute name beside its key type.
type Elements<'e> = &'e [(&'e str, &'e str)];

/// One row of the over-length table: the case, the table it names, and the schema it sends.
type OverLengthRow<'r> = (&'r str, &'r str, Elements<'r>);

/// One row of the definition table: the case, the table it names, the schema it sends, the
/// attributes the request declares, and the sentence that request earns — `None` where the
/// sentence is the one rendered over both lists ([`assert_undefined_keys`]), whose order is a
/// property of the NAMES rather than of the request, and `Some` for the sentences that
/// interpolate nothing ([`KEY_WITHOUT_DEFINITION`] and the role refusal).
type DefinitionRow<'r> = (
    &'r str,
    &'r str,
    Elements<'r>,
    &'r [&'r str],
    Option<&'r str>,
);

/// One `KeySchemaElement` body.
fn element(name: &str, key_type: &str) -> Value {
    json!({"AttributeName": name, "KeyType": key_type})
}

/// One `CreateTable` body over `key_schema`, declaring `definitions` as string attributes.
fn create_body(name: &str, key_schema: &[(&str, &str)], definitions: &[&str]) -> Value {
    json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": key_schema
            .iter()
            .map(|(attribute, key_type)| element(attribute, key_type))
            .collect::<Vec<Value>>(),
        "AttributeDefinitions": definitions
            .iter()
            .map(|attribute| json!({"AttributeName": attribute, "AttributeType": "S"}))
            .collect::<Vec<Value>>(),
    })
}

/// The attribute names a key schema names, which is the set a well-formed row declares.
fn attributes<'k>(key_schema: &[(&'k str, &str)]) -> Vec<&'k str> {
    key_schema.iter().map(|(attribute, _)| *attribute).collect()
}

/// A submitted `KeySchema` as the refusal of that member echoes it: the object rendering a
/// refused structure value carries, over the members in the order the model declares them
/// (`smithy_query(query="shape", shape="KeySchemaElement")` gives `AttributeName` then
/// `KeyType`).
fn rendered(key_schema: &[(&str, &str)]) -> String {
    format!(
        "[{}]",
        key_schema
            .iter()
            .map(|(attribute, key_type)| format!(
                "KeySchemaElement(attributeName={attribute}, keyType={key_type})"
            ))
            .collect::<Vec<String>>()
            .join(", ")
    )
}

/// The refusal a key schema past the two elements a primary key admits earns.
fn over_length(key_schema: &[(&str, &str)]) -> String {
    format!(
        "1 validation error detected: Value '{}' at 'keySchema' failed to satisfy constraint: \
         Member must have length less than or equal to 2",
        rendered(key_schema)
    )
}

/// AWS's fixed sentence for a declaration that cannot have one attribute definition per key
/// element — it carries fewer definitions than the schema has elements — spelled out here
/// rather than taken from the crate, so a reworded refusal fails this suite.
const KEY_WITHOUT_DEFINITION: &str = "Invalid KeySchema: Some index key attribute have no definition";

/// AWS's sentence for a key element the request declares no definition for, up to the two
/// bracketed lists that follow it: the whole key schema at fault, then every declared
/// attribute name. Stated for a request that declared at least as many definitions as the
/// schema has elements.
const UNDEFINED_KEY_PREFIX: &str =
    "One or more parameter values were invalid: Some index key attributes are not defined in \
     AttributeDefinitions. Keys: [";

/// Assert `said` is that sentence, over exactly the key schema's attribute names and exactly
/// the declared ones. The ORDER each list is rendered in is a property of the names, pinned
/// where that rendering is pinned ([`super::key_schema_and_vector_parameter_refusals`]), so
/// this compares the two lists as sets.
fn assert_undefined_keys(case: &str, said: &str, key_schema: Elements<'_>, declared: &[&str]) {
    assert!(
        said.starts_with(UNDEFINED_KEY_PREFIX),
        "case {case:?}: expected the parameter-value template, got {said:?}"
    );
    let bracketed = |at: usize| -> Vec<String> {
        said.split('[')
            .nth(at + 1)
            .and_then(|rest| rest.split(']').next())
            .unwrap_or_default()
            .split(", ")
            .filter(|name| !name.is_empty())
            .map(|name| name.to_string())
            .collect()
    };
    let sorted = |mut names: Vec<String>| {
        names.sort();
        names
    };
    let expected_keys: Vec<String> = attributes(key_schema)
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let expected_declared: Vec<String> = declared.iter().map(|name| (*name).to_string()).collect();
    assert_eq!(
        (sorted(bracketed(0)), sorted(bracketed(1))),
        (sorted(expected_keys), sorted(expected_declared)),
        "case {case:?}: expected the whole key schema beside every declared name, got {said:?}"
    );
}

/// Drive `CreateTable` with `body` and answer the (status, error code, message) triple a
/// refusal is read as.
async fn refusal(router: &MultiServiceRouter, body: Value) -> (u16, String, String) {
    let (status, parsed) = call(router, "CreateTable", body).await;
    (status, error_code(&parsed), message(&parsed))
}

/// Whether `ListTables` reports a table under `name` — RULE 3's reading.
async fn listed(router: &MultiServiceRouter, name: &str) -> bool {
    ok(router, "ListTables", json!({}))
        .await["TableNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .contains(&json!(name))
}

/// RULE 1 and RULE 3: rows differ in how many elements the schema carries, which roles they
/// claim and how the attributes are spelt.
#[tokio::test]
async fn a_primary_key_past_the_two_elements_it_admits_is_refused_over_the_submitted_list() {
    let router = emulator();
    let rows: [OverLengthRow<'_>; 4] = [
        (
            "one element past the bound",
            "KeySchemaThreeElements",
            &[("pk", "HASH"), ("sk", "RANGE"), ("extra", "HASH")],
        ),
        (
            "two past it, every element claiming the partition role",
            "KeySchemaFourHashes",
            &[("a", "HASH"), ("b", "HASH"), ("c", "HASH"), ("d", "HASH")],
        ),
        (
            "two past it, the sort role repeated",
            "KeySchemaThreeSorts",
            &[("a", "HASH"), ("b", "RANGE"), ("c", "RANGE"), ("d", "RANGE")],
        ),
        (
            "attribute names carrying every character the member admits",
            "KeySchemaSpeltNames",
            &[
                ("tenant.id", "HASH"),
                ("item-id", "RANGE"),
                ("third_col", "RANGE"),
            ],
        ),
    ];

    for (case, table, key_schema) in rows {
        let answered = refusal(
            &router,
            create_body(table, key_schema, &attributes(key_schema)),
        )
        .await;
        assert_eq!(
            answered,
            (
                400,
                "ValidationException".to_string(),
                over_length(key_schema)
            ),
            "case {case:?}: expected the length constraint over the submitted list, got \
             {answered:?}"
        );
        assert!(
            !listed(&router, table).await,
            "case {case:?}: a refused create stores no table under {table:?}"
        );
    }

    // The control: the two lengths a primary key does admit are served, so the refusals
    // above are about the LENGTH of the list and not about its elements.
    for (case, table, key_schema) in [
        ("a simple primary key", "KeySchemaOneElement", &[("pk", "HASH")][..]),
        (
            "a composite primary key",
            "KeySchemaTwoElements",
            &[("pk", "HASH"), ("sk", "RANGE")][..],
        ),
    ] {
        ok(
            &router,
            "CreateTable",
            create_body(table, key_schema, &attributes(key_schema)),
        )
        .await;
        let described = describe_when_active(&router, case, table).await;
        assert_eq!(
            described["KeySchema"],
            json!(key_schema
                .iter()
                .map(|(attribute, key_type)| element(attribute, key_type))
                .collect::<Vec<Value>>()),
            "case {case:?}: expected the key schema it was created with, got {described}"
        );
    }
}

/// RULE 2: rows differ in which element has no definition, what key type that element
/// claims, how many definitions the request declares against the number of elements the
/// schema has, and whether the request also declares a definition no key schema uses.
#[tokio::test]
async fn a_key_element_with_no_definition_is_refused_before_the_role_it_claims_is_read() {
    let router = emulator();
    let rows: [DefinitionRow<'_>; 8] = [
        (
            // The recorded case: one definition for a two-element schema.
            "a sort key with no definition, in the role its position requires",
            "KeyRoleDefinedFirst",
            &[("pk", "HASH"), ("sk", "RANGE")],
            &["pk"],
            Some(KEY_WITHOUT_DEFINITION),
        ),
        (
            "a second element claiming the partition role, with no definition",
            "KeyRoleSecondHash",
            &[("pk", "HASH"), ("other", "HASH")],
            &["pk"],
            Some(KEY_WITHOUT_DEFINITION),
        ),
        (
            // The same schema with one more definition declared: the request has stated
            // enough of them for the schema, so both lists are reported.
            "the same, with a definition no key schema uses declared beside it",
            "KeyRoleSecondHashAndExtra",
            &[("pk", "HASH"), ("other", "HASH")],
            &["pk", "unused"],
            None,
        ),
        (
            "a schema with more definitions declared than it has elements",
            "KeyRoleSpareDefinitions",
            &[("pk", "HASH"), ("other", "RANGE")],
            &["pk", "spare", "another"],
            None,
        ),
        (
            "a one-element schema with one definition, of another name",
            "KeyRoleOneForOne",
            &[("only", "HASH")],
            &["elsewhere"],
            None,
        ),
        (
            "the FIRST element claiming the sort role, with no definition",
            "KeyRoleFirstRange",
            &[("only", "RANGE")],
            &[],
            Some(KEY_WITHOUT_DEFINITION),
        ),
        (
            "a two-element schema the request declares nothing for",
            "KeyRoleNothingDeclared",
            &[("pk", "HASH"), ("sk", "RANGE")],
            &[],
            Some(KEY_WITHOUT_DEFINITION),
        ),
        (
            // Every element is declared, so what is left to read is the ROLE each one
            // claims — the refusal that names the element.
            "a second element claiming the partition role, every element declared",
            "KeyRoleSecondHashDeclared",
            &[("pk", "HASH"), ("other", "HASH")],
            &["pk", "other"],
            Some("Invalid KeySchema: The second KeySchemaElement is not a RANGE key type"),
        ),
    ];

    for (case, table, key_schema, definitions, expected) in rows {
        let (status, code, said) = refusal(&router, create_body(table, key_schema, definitions)).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected a 400 ValidationException, got {status} {code} {said:?}"
        );
        match expected {
            Some(sentence) => assert_eq!(
                said, sentence,
                "case {case:?}: expected {sentence:?}, got {said:?}"
            ),
            None => assert_undefined_keys(case, &said, key_schema, definitions),
        }
        assert!(
            !listed(&router, table).await,
            "case {case:?}: a refused create stores no table under {table:?}"
        );
    }
}

/// One index key schema as a row builds it: each element's attribute name beside its role.
type IndexElements = Vec<(String, &'static str)>;

/// One row of the index-count table: the case, the table it names, the index key schema it
/// sends, and the sentence that request earns.
type IndexCountRow<'r> = (&'r str, &'r str, IndexElements, String);

/// The attribute names and roles of an index key schema with `hash` partition keys followed
/// by `sort` sort keys.
fn index_key(hash: usize, sort: usize) -> IndexElements {
    (0..hash)
        .map(|at| (format!("h{at}"), "HASH"))
        .chain((0..sort).map(|at| (format!("r{at}"), "RANGE")))
        .collect()
}

/// A `CreateTable` body whose table keys on `pk` and which declares one secondary index of
/// `member` over `key_schema`.
fn indexed_body(table: &str, member: &str, key_schema: &[(String, &str)]) -> Value {
    let mut definitions = vec![json!({"AttributeName": "pk", "AttributeType": "S"})];
    for (attribute, _) in key_schema {
        if attribute != "pk" {
            definitions.push(json!({"AttributeName": attribute, "AttributeType": "S"}));
        }
    }
    let mut body = json!({
        "TableName": table,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": definitions,
    });
    body[member] = json!([{
        "IndexName": "idx",
        "KeySchema": key_schema
            .iter()
            .map(|(attribute, key_type)| element(attribute, key_type))
            .collect::<Vec<Value>>(),
        "Projection": {"ProjectionType": "ALL"},
    }]);
    body
}

/// The fixed sentence a key schema past one role's count earns.
fn exceeds_count(key_type: &str) -> String {
    format!(
        "One or more parameter values were invalid: The KeySchema exceeds the maximum allowed \
         number of {key_type} key attributes"
    )
}

/// RULE 3: rows differ in which role's count is exceeded, by how much, and whether the
/// index's kind admits several keys of that role at all.
#[tokio::test]
async fn an_index_key_schema_past_a_roles_count_states_that_counts_own_sentence() {
    let router = emulator();
    // The counts a GLOBAL secondary index admits, from the member's own documentation.
    const INDEX_PARTITION_KEYS: usize = 4;
    const INDEX_SORT_KEYS: usize = 4;
    let cases: [IndexCountRow<'_>; 5] = [
        (
            "one partition key past the count",
            "GlobalOneHashPast",
            index_key(INDEX_PARTITION_KEYS + 1, 0),
            exceeds_count("HASH"),
        ),
        (
            "two partition keys past it, with sort keys beside them",
            "GlobalTwoHashesPast",
            index_key(INDEX_PARTITION_KEYS + 2, 1),
            exceeds_count("HASH"),
        ),
        (
            "one sort key past the count",
            "GlobalOneSortPast",
            index_key(1, INDEX_SORT_KEYS + 1),
            exceeds_count("RANGE"),
        ),
        (
            "one past the partition count, with the rest of the schema full of sort keys",
            "GlobalHashPastWithSorts",
            index_key(INDEX_PARTITION_KEYS + 1, INDEX_SORT_KEYS - 1),
            exceeds_count("HASH"),
        ),
        (
            // A LOCAL index admits one key of each role, so a second partition key is an
            // element in the wrong ROLE for its position and is named by its ordinal — the
            // refusal the table's own primary key earns for the same schema.
            "a local index with a second partition key",
            "LocalSecondHash",
            vec![("pk".to_string(), "HASH"), ("other".to_string(), "HASH")],
            "Invalid KeySchema for index idx: The second KeySchemaElement is not a RANGE key \
             type"
                .to_string(),
        ),
    ];

    for (case, table, key_schema, expected) in cases {
        let member = if case.starts_with("a local") {
            "LocalSecondaryIndexes"
        } else {
            "GlobalSecondaryIndexes"
        };
        let answered = refusal(&router, indexed_body(table, member, &key_schema)).await;
        assert_eq!(
            answered,
            (400, "ValidationException".to_string(), expected.clone()),
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
    }

    // The control: both counts at their maximum are served, so the refusals above are
    // about exceeding a count and not about carrying several keys of a role.
    let key_schema = index_key(INDEX_PARTITION_KEYS, INDEX_SORT_KEYS);
    ok(
        &router,
        "CreateTable",
        indexed_body("GlobalAtBothCounts", "GlobalSecondaryIndexes", &key_schema),
    )
    .await;
    let described = describe_when_active(&router, "both counts at maximum", "GlobalAtBothCounts")
        .await;
    assert_eq!(
        crate::emulator::index_named(&described, "idx")["KeySchema"],
        json!(key_schema
            .iter()
            .map(|(attribute, key_type)| element(attribute, key_type))
            .collect::<Vec<Value>>()),
        "case \"both counts at maximum\": expected the index key schema it was created with, got \
         {described}"
    );
}

/// RULE 4: the two clauses of one refusal are stated in the order the service states those
/// members, whichever order the request wrote them in.
#[tokio::test]
async fn the_capacity_modes_clause_is_stated_before_the_key_schemas() {
    let router = emulator();
    let expected = "2 validation errors detected: Value 'BOGUS_MODE' at 'billingMode' failed to \
                    satisfy constraint: Member must satisfy enum value set: [PROVISIONED, \
                    PAY_PER_REQUEST]; Value 'BOGUS_ROLE' at 'keySchema.2.member.keyType' failed \
                    to satisfy constraint: Member must satisfy enum value set: [HASH, RANGE]";
    // Rows differ in the order the REQUEST writes the two members, which the order of the
    // clauses does not follow.
    for case in ["the mode first", "the key schema first"] {
        let mut body = json!({
            "TableName": "ClauseOrder",
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
            ],
        });
        if case == "the mode first" {
            body["BillingMode"] = json!("BOGUS_MODE");
            body["KeySchema"] = json!([element("pk", "HASH"), element("sk", "BOGUS_ROLE")]);
        } else {
            body["KeySchema"] = json!([element("pk", "HASH"), element("sk", "BOGUS_ROLE")]);
            body["BillingMode"] = json!("BOGUS_MODE");
        }
        let answered = refusal(&router, body).await;
        assert_eq!(
            answered,
            (400, "ValidationException".to_string(), expected.to_string()),
            "case {case:?}: expected the mode's clause first, got {answered:?}"
        );
    }
}
