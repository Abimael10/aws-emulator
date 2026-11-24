// SPDX-License-Identifier: CC0-1.0

//! What a request reports when a resource model's primary key is declared over an attribute
//! the model itself defines nowhere.
//!
//! The rule every case here holds the service to: **a requirement a resource model states
//! about ITSELF is settled by the type's handler, before the owning service is asked
//! anything, and a break in it terminates the request `FAILED` with `ErrorCode`
//! `InvalidRequest` — "the resource handler has returned that invalid input from the user has
//! generated a generic exception" (`smithy_query(query="doc",shape="InvalidRequestException")`)
//! — and the handler's OWN fixed
//! diagnosis as the `StatusMessage`, with nothing of the request and nothing of the owning
//! service's own exception wording in it.**
//!
//! For `AWS::DynamoDB::Table` that requirement is the contract of its two key properties:
//! "the attributes in `KeySchema` must also be defined in the `AttributeDefinitions` array"
//! (`CreateTable$KeySchema`). So a model whose key names an undefined attribute is a fault
//! in the DESIRED STATE, not a downstream refusal relayed onto the progress event — which is
//! what `downstream_refusal_reporting` and `dynamodb_table_read_through` assert the rest of
//! the pair's rules still are, each of them a rule only the owning service holds.
//!
//! The rows differ along what the rule generalises over: which key role names the undefined
//! attribute, how many of the key's elements do, how many attributes the model declares
//! (down to none at all, and to no `AttributeDefinitions` property), which scalar type those
//! it does declare carry, and the capacity mode the table was asked for. The one sentence
//! every row expects is the same, because that sentence has no variable parts.

use serde_json::{json, Value};

use super::emulator::{progress, text, Emulator, STATUS_FAILED, STATUS_SUCCESS};

/// The type whose resources live in the Amazon DynamoDB service's own store.
const TABLE: &str = "AWS::DynamoDB::Table";

/// The handler's own diagnosis of a key declared over an attribute the model does not
/// define. Entirely fixed text: no attribute name, table name, request token or id of the
/// request it refuses appears in it.
pub(crate) const KEY_ATTRIBUTE_UNDEFINED: &str =
    "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions";

/// The handler code a fault in the model is reported under.
const MODEL_FAULT: &str = "InvalidRequest";

/// The owning service's own diagnosis of the same declaration, which is the vocabulary this
/// API must NOT answer in: the fixed half of the `CreateTable` validation message that
/// service states for it (`service_com_amazonaws_dynamodb_dynamodb_20120810`'s own
/// `CreateTable` refusal), quoted here only so a case can assert its absence.
const OWNING_SERVICE_WORDING: &str = "Some index key attributes are not defined in";

#[tokio::test]
async fn a_key_over_an_undefined_attribute_carries_the_handlers_own_diagnosis() {
    let emulator = Emulator::with_dynamodb();
    // (case, table name, desired state, the attribute names no part of the message may quote)
    let rows: Vec<(&str, &str, Value, Vec<&str>)> = vec![
        (
            "the partition key names an attribute declared under another name",
            "diagnosis-hash-undefined",
            json!({
                "TableName": "diagnosis-hash-undefined",
                "KeySchema": [{ "AttributeName": "nosuch", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            vec!["nosuch", "pk"],
        ),
        (
            "the sort key names an undefined attribute while the partition key is defined",
            "diagnosis-range-undefined",
            json!({
                "TableName": "diagnosis-range-undefined",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "absent", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            vec!["absent", "pk"],
        ),
        (
            "both key elements name undefined attributes",
            "diagnosis-both-undefined",
            json!({
                "TableName": "diagnosis-both-undefined",
                "KeySchema": [
                    { "AttributeName": "partition", "KeyType": "HASH" },
                    { "AttributeName": "sort", "KeyType": "RANGE" },
                ],
                // Two declarations of the numeric and binary scalar types, neither of which
                // the key names: the model declares attributes, just not these.
                "AttributeDefinitions": [
                    { "AttributeName": "count", "AttributeType": "N" },
                    { "AttributeName": "blob", "AttributeType": "B" },
                ],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            vec!["partition", "sort", "count", "blob"],
        ),
        (
            "the model declares an empty attribute list",
            "diagnosis-no-definitions",
            json!({
                "TableName": "diagnosis-no-definitions",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            vec!["id"],
        ),
        (
            "the model carries no attribute list at all",
            "diagnosis-definitions-absent",
            json!({
                "TableName": "diagnosis-definitions-absent",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            vec!["id"],
        ),
        (
            "a provisioned table whose key names an undefined attribute",
            "diagnosis-provisioned",
            json!({
                "TableName": "diagnosis-provisioned",
                "KeySchema": [{ "AttributeName": "missing", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "present", "AttributeType": "N" }],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
            }),
            vec!["missing", "present"],
        ),
    ];

    // Started together, then waited for: the creates are independent, so one span covers
    // them all.
    let mut accepted = Vec::new();
    for (case, _, desired, _) in &rows {
        let (status, body) = emulator.create(TABLE, desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the model is valid against the type's schema, so the create must \
             be accepted, got {status} with body {body:?}"
        );
        accepted.push(text(&progress(&body), "RequestToken"));
    }

    for ((case, name, _, request_values), request_token) in rows.iter().zip(&accepted) {
        let settled = emulator.terminal(request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            MODEL_FAULT,
            "case {case:?}: expected the {MODEL_FAULT} handler verdict, got {settled:?}"
        );
        let message = text(&settled, "StatusMessage");
        assert_eq!(
            message, KEY_ATTRIBUTE_UNDEFINED,
            "case {case:?}: expected {KEY_ATTRIBUTE_UNDEFINED:?}, got {message:?}"
        );
        // The sentence is the handler's, not the owning service's, and it quotes nothing the
        // request carried.
        assert!(
            !message.contains(OWNING_SERVICE_WORDING),
            "case {case:?}: the owning service's own wording must not be relayed, got \
             {message:?}"
        );
        for value in request_values {
            assert!(
                !message.contains(value),
                "case {case:?}: the message must interpolate nothing from the request, but it \
                 carries {value:?}: {message:?}"
            );
        }
        assert!(
            !message.contains(name) && !message.contains(request_token.as_str()),
            "case {case:?}: the message must carry neither the table's name nor the request \
             token, got {message:?}"
        );

        // Nothing was written, in either plane: the refused create leaves no table behind,
        // because the handler judged the model before it called the owning service.
        let (status, body) = emulator
            .dynamodb("DescribeTable", json!({ "TableName": name }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the refused create must leave no table, got {status} with body \
             {body:?}"
        );
        let listed = emulator
            .dynamodb_ok("ListTables", json!({}))
            .await
            .get("TableNames")
            .cloned()
            .unwrap_or(Value::Null);
        assert!(
            !listed
                .as_array()
                .is_some_and(|names| names.iter().any(|held| held.as_str() == Some(*name))),
            "case {case:?}: the owning service must hold no such table, got {listed:?}"
        );
        let (status, body) = emulator.get(TABLE, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the refused create must leave no resource, got {status} with body \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn a_key_the_model_declares_its_attributes_for_is_created_and_usable() {
    let emulator = Emulator::with_dynamodb();
    // The control for the rule above: the check is of the model's own consistency and of
    // nothing else, so every key whose attributes the model DOES declare is provisioned —
    // whichever roles it names, whichever scalar types they carry and whichever capacity mode
    // the table is in — and the table it leaves behind is the owning service's own, item
    // writes included.
    let rows: Vec<(&str, &str, Value, Value)> = vec![
        (
            "a partition key over a string attribute",
            "declared-simple",
            json!({
                "TableName": "declared-simple",
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            json!({ "pk": { "S": "one" } }),
        ),
        (
            "a composite key over a string and a numeric attribute",
            "declared-composite",
            json!({
                "TableName": "declared-composite",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "at", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "at", "AttributeType": "N" },
                ],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 2,
                    "WriteCapacityUnits": 3,
                },
            }),
            json!({ "pk": { "S": "one" }, "at": { "N": "7" } }),
        ),
    ];

    for (case, name, desired, item) in &rows {
        let accepted = emulator.create_ok(TABLE, desired, None).await;
        let settled = emulator.settled(&text(&accepted, "RequestToken")).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: a model whose key is declared must be provisioned, got {settled:?}"
        );

        // Read back through this API: the resource answers to the table's own name.
        let (status, body) = emulator.get(TABLE, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetResource expected HTTP 200, got {status} with body {body:?}"
        );
        assert_eq!(
            text(&body["ResourceDescription"], "Identifier"),
            *name,
            "case {case:?}: expected the table's own name, got {body:?}"
        );

        // And in the owning service's own plane, with the key the model declared — a table
        // that takes an item on that key is one that was really created there.
        let described = emulator
            .dynamodb_ok("DescribeTable", json!({ "TableName": name }))
            .await;
        assert_eq!(
            described["Table"]["KeySchema"], desired["KeySchema"],
            "case {case:?}: expected the key the model declared, got {described:?}"
        );
        emulator
            .dynamodb_ok("PutItem", json!({ "TableName": name, "Item": item }))
            .await;
        let read = emulator
            .dynamodb_ok("GetItem", json!({ "TableName": name, "Key": item }))
            .await;
        assert_eq!(
            read["Item"], *item,
            "case {case:?}: expected the item the table was written, got {read:?}"
        );
    }
}
