// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a request earns when several of its facts are refusable at once, driven end
//! to end over the two surfaces where the order decides the error CLASS a client branches
//! on.
//!
//! RULE 1 — a rule about the TRANSACTION as a whole is answered only once every one of its
//! elements has been resolved against the table it names, so a transaction that names a
//! table this account does not hold is refused for the missing table (`ResourceNotFound`)
//! whichever element names it, even when another pair of elements addresses one item
//! (`ValidationException`). The rows span the surface (`TransactGetItems`,
//! `TransactWriteItems`), the position of the element naming the absent table, the number of
//! elements, the absent table's name and the actions the elements state; the controls show
//! each class is still served on its own.
//!
//! RULE 2 — a rule about the REQUEST's own members is settled before the table is looked up,
//! so an `UpdateTable` whose global-secondary-index create does not declare its key
//! attributes in `AttributeDefinitions` earns that validation refusal whether or not the
//! table it names exists. The rows span the form the declaration is missing in (no member at
//! all, a member naming other attributes), the index's name, the arity of its key schema and
//! whether the table is there; the controls show a complete create is served and a missing
//! table is still reported for a request with nothing else wrong.
//!
//! Every assertion names its case and the value it actually read.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row of this suite that needs one addresses.
const TABLE: &str = "RefusalClassPrecedence";

/// Two table names nothing here creates, which is what makes the existence check decide.
const ABSENT: [&str; 2] = ["NoSuchPrecedenceTable", "AnotherAbsentPrecedenceTable"];

/// AWS's fixed data-plane sentence for a table that is not there, which the item surfaces
/// state without naming the table.
const NOT_FOUND: &str = "Requested resource not found";

/// AWS's sentence for a transaction that addresses one item twice.
const DUPLICATE: &str = state::DUPLICATE_TRANSACT_ITEM_MESSAGE;

/// An emulator holding `TABLE`, keyed on one string partition attribute.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    router
}

/// A `Get` element addressing `key` of `table`.
fn get(table: &str, key: &str) -> Value {
    json!({"Get": {"TableName": table, "Key": {"pk": {"S": key}}}})
}

/// A `Put` element writing `key` into `table`.
fn put(table: &str, key: &str) -> Value {
    json!({"Put": {"TableName": table, "Item": {"pk": {"S": key}}}})
}

/// A `Delete` element removing `key` from `table`.
fn delete(table: &str, key: &str) -> Value {
    json!({"Delete": {"TableName": table, "Key": {"pk": {"S": key}}}})
}

/// The status, error code and message one call answers.
async fn answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, String, String) {
    let (status, parsed) = call(router, operation, body).await;
    (status, error_code(&parsed), message(&parsed))
}

#[tokio::test]
async fn a_transaction_naming_an_absent_table_is_refused_for_the_table_not_for_its_repeats() {
    let router = seeded().await;

    // Rows differ in the surface, in where the element naming the absent table sits relative
    // to the repeated pair, in how many elements the transaction carries, in which absent
    // table is named and in the actions the elements state. Every row ALSO addresses one
    // item twice, which is the refusal the transaction would earn if the structural rule
    // outranked table resolution.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a read whose last element names the absent table",
            "TransactGetItems",
            json!([get(TABLE, "a"), get(TABLE, "a"), get(ABSENT[0], "b")]),
        ),
        (
            "a read whose middle element names the absent table",
            "TransactGetItems",
            json!([get(TABLE, "a"), get(ABSENT[1], "b"), get(TABLE, "a")]),
        ),
        (
            "a read whose first element names the absent table",
            "TransactGetItems",
            json!([get(ABSENT[0], "b"), get(TABLE, "a"), get(TABLE, "a")]),
        ),
        (
            "a read of four elements, two repeats and one absent table",
            "TransactGetItems",
            json!([
                get(TABLE, "a"),
                get(TABLE, "c"),
                get(TABLE, "a"),
                get(ABSENT[1], "b"),
            ]),
        ),
        (
            "a write whose last element names the absent table",
            "TransactWriteItems",
            json!([put(TABLE, "a"), put(TABLE, "a"), put(ABSENT[0], "b")]),
        ),
        (
            "a write whose repeats are a put and a delete",
            "TransactWriteItems",
            json!([put(TABLE, "a"), delete(TABLE, "a"), put(ABSENT[1], "b")]),
        ),
        (
            "a write whose middle element names the absent table",
            "TransactWriteItems",
            json!([put(TABLE, "a"), delete(ABSENT[0], "b"), delete(TABLE, "a")]),
        ),
    ];

    for (case, operation, elements) in rows {
        let (status, code, said) = answered(
            &router,
            operation,
            json!({"TransactItems": elements.clone()}),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), said.as_str()),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?} ({operation}): the missing table outranks the repeated item, got \
             {status} {code} {said:?}"
        );
    }

    // CONTROL 1: the same repeats with every element naming a table that IS there earn the
    // structural refusal, so the rows above are the precedence and not a blanket answer.
    for (case, operation, elements) in [
        (
            "a read repeating one item",
            "TransactGetItems",
            json!([get(TABLE, "a"), get(TABLE, "a")]),
        ),
        (
            "a write repeating one item",
            "TransactWriteItems",
            json!([put(TABLE, "a"), delete(TABLE, "a")]),
        ),
        (
            "a write repeating the middle of three items",
            "TransactWriteItems",
            json!([put(TABLE, "a"), put(TABLE, "c"), delete(TABLE, "c")]),
        ),
    ] {
        let (status, code, said) =
            answered(&router, operation, json!({"TransactItems": elements})).await;
        assert_eq!(
            (status, code.as_str(), said.as_str()),
            (400, "ValidationException", DUPLICATE),
            "case {case:?} ({operation}): a transaction that only repeats an item is refused \
             for the repeat, got {status} {code} {said:?}"
        );
    }

    // CONTROL 2: distinct items of a table that is there are served, so neither refusal is
    // what this surface answers in general.
    ok(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [put(TABLE, "d"), put(TABLE, "e")]}),
    )
    .await;
    let read = ok(
        &router,
        "TransactGetItems",
        json!({"TransactItems": [get(TABLE, "d"), get(TABLE, "e")]}),
    )
    .await;
    assert_eq!(
        read["Responses"],
        json!([{"Item": {"pk": {"S": "d"}}}, {"Item": {"pk": {"S": "e"}}}]),
        "case \"two distinct items\": the transaction reads what it wrote, got {read}"
    );
}

/// The `AttributeDefinitions` sentence for an index whose key attributes the request
/// declares nothing for, which names the member the request has to carry and the index that
/// needs it.
fn definitions_not_specified(index: &str) -> String {
    format!(
        "One or more parameter values were invalid: AttributeDefinitions is not specified for \
         index: {index}"
    )
}

/// The sentence for an index whose key schema has more elements than the request declared
/// definitions, which names neither the index nor the attribute left undeclared.
const KEY_WITHOUT_DEFINITION: &str = "Invalid KeySchema: Some index key attribute have no \
                                      definition";

/// One `GlobalSecondaryIndexUpdates` entry creating `index` over `keys`.
fn create_index(index: &str, keys: &[(&str, &str)]) -> Value {
    let key_schema: Vec<Value> = keys
        .iter()
        .map(|(attribute, role)| json!({"AttributeName": attribute, "KeyType": role}))
        .collect();
    json!({"Create": {
        "IndexName": index,
        "KeySchema": key_schema,
        "Projection": {"ProjectionType": "ALL"},
    }})
}

#[tokio::test]
async fn an_index_create_missing_its_definitions_is_refused_whichever_table_it_named() {
    let router = seeded().await;

    // Rows differ in HOW the declaration is missing — the member left out entirely, and a
    // member declaring another attribute than the index's key — in the index's name, and in
    // the arity of its key schema. Each is a fact of the REQUEST's own two members, so each
    // row is driven against the table that is there AND against one that is not.
    let rows: Vec<(&str, Value, Value, String)> = vec![
        (
            "no AttributeDefinitions at all, one key element",
            Value::Null,
            create_index("by-gk", &[("gk", "HASH")]),
            definitions_not_specified("by-gk"),
        ),
        (
            "no AttributeDefinitions at all, another index name",
            Value::Null,
            create_index("gsi-ix7", &[("gk", "HASH")]),
            definitions_not_specified("gsi-ix7"),
        ),
        (
            "no AttributeDefinitions at all, a composite index key",
            Value::Null,
            create_index("by-gk-and-ts", &[("gk", "HASH"), ("ts", "RANGE")]),
            definitions_not_specified("by-gk-and-ts"),
        ),
        (
            "an empty AttributeDefinitions list",
            json!([]),
            create_index("by-gk", &[("gk", "HASH")]),
            definitions_not_specified("by-gk"),
        ),
        (
            "definitions naming neither element of a composite index key",
            json!([{"AttributeName": "pk", "AttributeType": "S"}]),
            create_index("by-gk-and-ts", &[("gk", "HASH"), ("ts", "RANGE")]),
            KEY_WITHOUT_DEFINITION.to_string(),
        ),
    ];

    for (case, definitions, entry, expected) in rows {
        for table in [TABLE, ABSENT[0]] {
            let mut body = json!({
                "TableName": table,
                "GlobalSecondaryIndexUpdates": [entry.clone()],
            });
            if !definitions.is_null() {
                body["AttributeDefinitions"] = definitions.clone();
            }
            let (status, code, said) = answered(&router, "UpdateTable", body).await;
            assert_eq!(
                (status, code.as_str(), said.as_str()),
                (400, "ValidationException", expected.as_str()),
                "case {case:?} against {table:?}: the request's own rule is answered whichever \
                 table it named, got {status} {code} {said:?}"
            );
        }
    }

    // CONTROL 1: a request with nothing wrong but the table still reports the missing table,
    // so the rows above are the precedence and not a blanket validation answer.
    for absent in ABSENT {
        let (status, code, said) = answered(
            &router,
            "UpdateTable",
            json!({
                "TableName": absent,
                "AttributeDefinitions": [{"AttributeName": "gk", "AttributeType": "S"}],
                "GlobalSecondaryIndexUpdates": [create_index("by-gk", &[("gk", "HASH")])],
            }),
        )
        .await;
        let expected = format!("Requested resource not found: Table: {absent} not found");
        assert_eq!(
            (status, code.as_str(), said.as_str()),
            (400, "ResourceNotFoundException", expected.as_str()),
            "case \"a complete create against an absent table\" ({absent}): expected the \
             missing table, got {status} {code} {said:?}"
        );
    }

    // CONTROL 2: the same complete create against the table that IS there is served, and the
    // index it asked for is read back off the table.
    let updated = ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{"AttributeName": "gk", "AttributeType": "S"}],
            "GlobalSecondaryIndexUpdates": [create_index("by-gk", &[("gk", "HASH")])],
        }),
    )
    .await;
    assert_eq!(
        updated["TableDescription"]["GlobalSecondaryIndexes"][0]["IndexName"],
        json!("by-gk"),
        "case \"a complete create\": the index is reported on the table, got {updated}"
    );
    let described = describe_when_active(&router, "the updated table", TABLE).await;
    assert_eq!(
        described["GlobalSecondaryIndexes"][0]["IndexName"],
        json!("by-gk"),
        "case \"a complete create\": the index is read back off the table, got {described}"
    );
}
