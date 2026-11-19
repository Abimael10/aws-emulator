// SPDX-License-Identifier: CC0-1.0

//! Which sentence a key schema earns for an element `AttributeDefinitions` does not
//! declare, end to end over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.
//!
//! The rule: a declaration's key schemas are resolved against the ONE
//! `AttributeDefinitions` list the request carries, and which refusal an undeclared key
//! attribute earns follows from the COUNT that list holds against the number of elements
//! the schema at fault has. A request that declared FEWER definitions than the schema has
//! elements cannot have one per element whatever the names say, so the fact stated is the
//! element left with no definition, in a sentence that interpolates nothing; a request that
//! declared at least as many stated enough definitions for the schema and got the NAMES
//! wrong, so it is told what each of the two lists holds — the whole key schema at fault
//! beside every declared attribute name — and has to reconcile them.
//!
//! The rule is about a KEY SCHEMA and not about an operation or a schema kind, so the rows
//! below span every schema a declaration carries: the table's own primary key, a global
//! secondary index's key and a local secondary index's key declared with the table, and the
//! key of an index an `UpdateTable` creates. They also span the dimension the rule turns on
//! — one fewer definition than elements, none at all, exactly as many, and more — and the
//! position of the element with nothing declared for it.
//!
//! Both sentences are written out here rather than taken from the renderer under test, and
//! the ORDER the two lists are rendered in is a property of the names, pinned where that
//! rendering is pinned (`super::key_schema_and_vector_parameter_refusals`), so this suite
//! compares the lists as sets.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, index_named, message, ok};

/// AWS's fixed sentence for a schema the request declared fewer attribute definitions than
/// it has elements for.
const KEY_WITHOUT_DEFINITION: &str = "Invalid KeySchema: Some index key attribute have no \
                                      definition";

/// The fixed text of AWS's sentence over the two lists, up to the first of them.
const UNDEFINED_KEY_PREFIX: &str =
    "One or more parameter values were invalid: Some index key attributes are not defined in \
     AttributeDefinitions. Keys: [";

/// One key schema as a row writes it: each element's attribute name beside its key type.
type Elements<'e> = &'e [(&'e str, &'e str)];

/// Which schema of the declaration names an attribute nothing declares.
#[derive(Clone, Copy, Debug)]
enum Schema {
    /// The table's own primary key.
    Table,
    /// A global secondary index declared with the table.
    Global,
    /// A local secondary index declared with the table, whose partition key is the table's.
    Local,
    /// A global secondary index an `UpdateTable` creates on a table that already exists.
    Added,
}

/// One row: the case, the schema at fault, that schema's elements, and the attribute names
/// the request declares.
struct Row<'r> {
    case: &'r str,
    schema: Schema,
    elements: Elements<'r>,
    declared: &'r [&'r str],
}

/// One `KeySchemaElement` body.
fn element(name: &str, key_type: &str) -> Value {
    json!({"AttributeName": name, "KeyType": key_type})
}

/// The `AttributeDefinitions` body declaring every name in `names` as a string attribute.
fn definitions(names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|name| json!({"AttributeName": name, "AttributeType": "S"}))
        .collect()
}

/// The elements of a key schema as the request sends them.
fn elements_of(schema: Elements<'_>) -> Vec<Value> {
    schema
        .iter()
        .map(|(name, key_type)| element(name, key_type))
        .collect()
}

/// The two bracketed lists of a `Keys: [..], AttributeDefinitions: [..]` sentence, as sets.
///
/// Each list is cut at the sentence's own fixed markers rather than at the nth bracket, so
/// a name carrying a bracket cannot shift which list a name is read into.
fn reported_lists(stated: &str) -> (Vec<String>, Vec<String>) {
    const KEYS_OPENS: &str = "Keys: [";
    const DEFINITIONS_OPENS: &str = "], AttributeDefinitions: [";
    let names = |section: &str| -> Vec<String> {
        let mut names: Vec<String> = section
            .split(", ")
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        names.sort();
        names
    };
    let after_keys = stated.split_once(KEYS_OPENS).map(|(_, rest)| rest);
    let (keys, rest) = after_keys
        .and_then(|rest| rest.split_once(DEFINITIONS_OPENS))
        .unwrap_or_default();
    let definitions = rest.split(']').next().unwrap_or_default();
    (names(keys), names(definitions))
}

/// The same names, each once, sorted — what a reported list is compared against.
fn as_set(names: &[&str]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        if !kept.contains(&(*name).to_string()) {
            kept.push((*name).to_string());
        }
    }
    kept.sort();
    kept
}

/// The extra global indexes a row needs so that every name it declares is CONSUMED by some
/// key schema: one index per declared name no schema of the row already uses.
///
/// `CreateTable` refuses a definition no key schema uses, which is a different rule and
/// would answer first for a row that declares spare names; soaking them up keeps each row
/// about the key with nothing declared for it. Every soaked name is declared, so each of
/// these indexes holds together.
fn soak_indexes(declared: &[&str], consumed: &[&str]) -> Vec<Value> {
    declared
        .iter()
        .filter(|name| !consumed.contains(name))
        .enumerate()
        .map(|(at, name)| {
            json!({
                "IndexName": format!("used-{at}"),
                "KeySchema": [element(name, "HASH")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            })
        })
        .collect()
}

/// Send the row's declaration and answer the (status, code, message) triple it is refused
/// with. A row whose schema is `Added` creates the table first, so the refusal is
/// `UpdateTable`'s.
async fn refusal(router: &MultiServiceRouter, table: &str, row: &Row<'_>) -> (u16, String, String) {
    let index = json!({
        "IndexName": "idx",
        "KeySchema": elements_of(row.elements),
        "Projection": {"ProjectionType": "ALL"},
    });
    // What the row's own schemas consume, which is what decides whether a declared name
    // needs soaking up ([`soak_indexes`]).
    let mut consumed: Vec<&str> = vec!["pk"];
    consumed.extend(row.elements.iter().map(|(name, _)| *name));
    let request = match row.schema {
        Schema::Table => json!({
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": elements_of(row.elements),
            "AttributeDefinitions": definitions(row.declared),
        }),
        Schema::Global => {
            let mut indexes = vec![index];
            indexes.extend(soak_indexes(row.declared, &consumed));
            json!({
                "TableName": table,
                "BillingMode": "PAY_PER_REQUEST",
                "KeySchema": [element("pk", "HASH")],
                "AttributeDefinitions": definitions(row.declared),
                "GlobalSecondaryIndexes": indexes,
            })
        }
        Schema::Local => {
            let mut request = json!({
                "TableName": table,
                "BillingMode": "PAY_PER_REQUEST",
                "KeySchema": [element("pk", "HASH")],
                "AttributeDefinitions": definitions(row.declared),
                "LocalSecondaryIndexes": [index],
            });
            // The member is left OUT when nothing needs soaking up: an empty
            // `GlobalSecondaryIndexes` list is a refusal of its own.
            let soaks = soak_indexes(row.declared, &consumed);
            if !soaks.is_empty() {
                request["GlobalSecondaryIndexes"] = json!(soaks);
            }
            request
        }
        Schema::Added => {
            // The table the update addresses, created from a declaration that holds
            // together, so what the update is refused for is its OWN definitions.
            ok(
                router,
                "CreateTable",
                json!({
                    "TableName": table,
                    "BillingMode": "PAY_PER_REQUEST",
                    "KeySchema": [element("pk", "HASH")],
                    "AttributeDefinitions": definitions(&["pk"]),
                }),
            )
            .await;
            describe_when_active(router, row.case, table).await;
            json!({
                "TableName": table,
                "AttributeDefinitions": definitions(row.declared),
                "GlobalSecondaryIndexUpdates": [{"Create": index}],
            })
        }
    };
    let operation = match row.schema {
        Schema::Added => "UpdateTable",
        _ => "CreateTable",
    };
    let (status, parsed) = call(router, operation, request).await;
    (status, error_code(&parsed), message(&parsed))
}

/// Every row whose request declares FEWER definitions than the schema at fault has
/// elements, spanning the schema kind, the position of the undeclared element and how far
/// short the list falls.
const TOO_FEW: &[Row<'_>] = &[
    Row {
        // The recorded case: one definition for a two-element primary key.
        case: "a table key with one definition for two elements",
        schema: Schema::Table,
        elements: &[("pk", "HASH"), ("sk", "RANGE")],
        declared: &["pk"],
    },
    Row {
        case: "a table key whose PARTITION key is the undeclared one",
        schema: Schema::Table,
        elements: &[("alpha", "HASH"), ("beta", "RANGE")],
        declared: &["beta"],
    },
    Row {
        case: "a table key the request declares nothing for",
        schema: Schema::Table,
        elements: &[("pk", "HASH"), ("sk", "RANGE")],
        declared: &[],
    },
    Row {
        case: "a one-element table key the request declares nothing for",
        schema: Schema::Table,
        elements: &[("only", "HASH")],
        declared: &[],
    },
    Row {
        case: "a global index key with one definition for two elements",
        schema: Schema::Global,
        elements: &[("gk", "HASH"), ("gs", "RANGE")],
        declared: &["pk"],
    },
    Row {
        case: "a local index key with one definition for two elements",
        schema: Schema::Local,
        elements: &[("pk", "HASH"), ("ls", "RANGE")],
        declared: &["pk"],
    },
    Row {
        case: "an added global index key with one definition for two elements",
        schema: Schema::Added,
        elements: &[("ak", "HASH"), ("as", "RANGE")],
        declared: &["ak"],
    },
];

/// Every row whose request declares at least as many definitions as the schema at fault has
/// elements, spanning the schema kind, the position of the undeclared element and how many
/// spare definitions the list carries.
const ENOUGH: &[Row<'_>] = &[
    Row {
        // The recorded case: as many definitions as elements, one of them unused.
        case: "a table key beside a definition no schema uses",
        schema: Schema::Table,
        elements: &[("PK", "HASH"), ("SK", "RANGE")],
        declared: &["PK", "EXTRA"],
    },
    Row {
        case: "a one-element table key with one definition of another name",
        schema: Schema::Table,
        elements: &[("only", "HASH")],
        declared: &["elsewhere"],
    },
    Row {
        case: "a table key with more definitions than elements",
        schema: Schema::Table,
        elements: &[("pk", "HASH"), ("sk", "RANGE")],
        declared: &["pk", "spare", "another"],
    },
    Row {
        case: "a global index key with as many definitions as elements",
        schema: Schema::Global,
        elements: &[("gk", "HASH"), ("gs", "RANGE")],
        declared: &["pk", "gk"],
    },
    Row {
        case: "a local index key with more definitions than elements",
        schema: Schema::Local,
        elements: &[("pk", "HASH"), ("ls", "RANGE")],
        declared: &["pk", "spare", "another"],
    },
    Row {
        case: "an added global index key with as many definitions as elements",
        schema: Schema::Added,
        elements: &[("ak", "HASH"), ("as", "RANGE")],
        declared: &["ak", "elsewhere"],
    },
];

/// Whether `ListTables` reports a table under `name`.
async fn listed(router: &MultiServiceRouter, name: &str) -> bool {
    ok(router, "ListTables", json!({}))
        .await["TableNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .contains(&json!(name))
}

#[tokio::test]
async fn too_few_definitions_states_the_element_left_without_one() {
    let router = emulator();

    for (ordinal, row) in TOO_FEW.iter().enumerate() {
        let table = format!("TooFew{ordinal}");
        let answered = refusal(&router, &table, row).await;
        assert_eq!(
            answered,
            (
                400,
                "ValidationException".to_string(),
                KEY_WITHOUT_DEFINITION.to_string()
            ),
            "case {:?}: expected the sentence for the element left without a definition, got \
             {answered:?}",
            row.case
        );
        // …and the refused declaration built nothing: a refused create stores no table,
        // and a refused update adds no index to the one it addressed.
        match row.schema {
            Schema::Added => {
                let described = describe_when_active(&router, row.case, &table).await;
                assert_eq!(
                    index_named(&described, "idx"),
                    Value::Null,
                    "case {:?}: a refused update adds no index, got {described}",
                    row.case
                );
            }
            _ => assert!(
                !listed(&router, &table).await,
                "case {:?}: a refused create stores no table under {table:?}",
                row.case
            ),
        }
    }
}

#[tokio::test]
async fn enough_definitions_states_both_lists() {
    let router = emulator();

    for (ordinal, row) in ENOUGH.iter().enumerate() {
        let table = format!("Enough{ordinal}");
        let (status, code, said) = refusal(&router, &table, row).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {:?}: expected a 400 ValidationException, got {status} {code} {said:?}",
            row.case
        );
        assert!(
            said.starts_with(UNDEFINED_KEY_PREFIX),
            "case {:?}: expected the parameter-value sentence over both lists, got {said:?}",
            row.case
        );
        // The first list is the WHOLE key schema at fault, the second every declared name.
        let keys: Vec<&str> = row.elements.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            reported_lists(&said),
            (as_set(&keys), as_set(row.declared)),
            "case {:?}: expected the schema's own keys beside every declared name, got \
             {said:?}",
            row.case
        );
        match row.schema {
            Schema::Added => {
                let described = describe_when_active(&router, row.case, &table).await;
                assert_eq!(
                    index_named(&described, "idx"),
                    Value::Null,
                    "case {:?}: a refused update adds no index, got {described}",
                    row.case
                );
            }
            _ => assert!(
                !listed(&router, &table).await,
                "case {:?}: a refused create stores no table under {table:?}",
                row.case
            ),
        }
    }
}

/// The control: a declaration whose every key element IS declared is served, and the stored
/// table reports each schema back — so the refusals above are about a key with no
/// definition and not about the shape of these declarations.
#[tokio::test]
async fn a_declaration_whose_every_key_element_is_declared_is_served() {
    let router = emulator();

    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "EveryKeyDeclared",
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [element("pk", "HASH"), element("sk", "RANGE")],
            "AttributeDefinitions": definitions(&["pk", "sk", "gk", "ls"]),
            "GlobalSecondaryIndexes": [{
                "IndexName": "by-gk",
                "KeySchema": [element("gk", "HASH")],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "LocalSecondaryIndexes": [{
                "IndexName": "by-ls",
                "KeySchema": [element("pk", "HASH"), element("ls", "RANGE")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
        }),
    )
    .await;
    let described = describe_when_active(&router, "every key declared", "EveryKeyDeclared").await;
    assert_eq!(
        described["KeySchema"],
        json!([element("pk", "HASH"), element("sk", "RANGE")]),
        "case \"every key declared\": expected the primary key it was created with, got \
         {described}"
    );
    assert_eq!(
        index_named(&described, "by-gk")["KeySchema"],
        json!([element("gk", "HASH")]),
        "case \"every key declared\": expected the global index's key schema, got {described}"
    );

    // …and an index an update CREATES, whose key the update declares, is added.
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "EveryKeyDeclared",
            "AttributeDefinitions": definitions(&["ak"]),
            "GlobalSecondaryIndexUpdates": [{"Create": {
                "IndexName": "by-ak",
                "KeySchema": [element("ak", "HASH")],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }}],
        }),
    )
    .await;
    let described = describe_when_active(&router, "an added index", "EveryKeyDeclared").await;
    assert_eq!(
        index_named(&described, "by-ak")["KeySchema"],
        json!([element("ak", "HASH")]),
        "case \"an added index\": expected the added index's key schema, got {described}"
    );
}
