// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the tables Amazon DynamoDB holds.
//!
//! The rule every case here holds the service to: **a resource of a type another service of
//! this emulator owns is resolved, enumerated, provisioned, patched and removed in THAT
//! service's own state, whichever API created it — "you can use this action to return
//! information about an existing resource in your account and Amazon Web Services Region,
//! whether those resources were provisioned using Cloud Control API" (`GetResource`) — and it
//! answers to the identifier the type's schema declares.** For `AWS::DynamoDB::Table` that
//! identifier is the table's own name (`primaryIdentifier` `/properties/TableName`), so a
//! table `dynamodb:CreateTable` created is one `GetResource` reports under that name, and one
//! `dynamodb:DeleteTable` removed is one it reports as not found.
//!
//! The rows differ along what the rule generalises over: which API the table came into being
//! through, the shape of its primary key, its capacity mode and capacity, the indexes and the
//! stream it carries, its storage class, its deletion protection and its tag set.
//!
//! `sqs_queue_read_through`, `sns_topic_read_through`, `ssm_parameter_read_through` and
//! `native_resource_resolution` hold the same rule for the `AWS::SQS::*`, `AWS::SNS::*`,
//! `AWS::SSM::*` and `AWS::Logs::*` types, so what is asserted across the suites is where a
//! resource LIVES rather than anything about one owning service.

use std::time::Instant;

use serde_json::{json, Map, Value};

use super::emulator::{
    code, descriptions, message, progress, properties_of, text, Emulator, SETTLED_BUDGET,
    SETTLED_INTERVAL, SIGNING_REGION, STATUS_FAILED,
};
use super::refusal_wording::assert_handler_not_found;

/// The type whose resources live in the Amazon DynamoDB service's own store.
const TABLE: &str = "AWS::DynamoDB::Table";

/// The ARN of the table called `name`, built by the OWNING service's own ARN builder so a
/// case pins the ARN Cloud Control API reported to the one that service holds rather than to
/// a string this suite spells.
fn table_arn(name: &str) -> String {
    service_com_amazonaws_dynamodb_dynamodb_20120810::state::table_arn(SIGNING_REGION, name)
}

/// The error code Amazon DynamoDB answers for a table it does not hold
/// (`ResourceNotFoundException`).
const TABLE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The not-found diagnostic the handler that owns this type states, which every Cloud
/// Control API refusal about an absent table carries. Catalog source: the refusal the
/// `AWS::DynamoDB::Table` handler is recorded answering through this API, which NAMES the
/// table it holds none of.
fn handler_not_found(name: &str) -> String {
    format!("Table: {name} does not exist.")
}

/// The `TableDescription` the owning service's own read reports for `name`, required to be
/// answered.
async fn described(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (status, body) = emulator
        .dynamodb("DescribeTable", json!({ "TableName": name }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: dynamodb:DescribeTable expected HTTP 200, got {status} with body \
         {body:?}"
    );
    body["Table"].clone()
}

/// Wait until the owning service reports `name` as `ACTIVE`, answering its description then.
///
/// The wait is for the STATUS and never for a span: a newly created table is `CREATING`
/// until the clock owes it the transition, so this re-reads until the condition holds or the
/// generous deadline the harness shares passes.
async fn active(emulator: &Emulator, case: &str, name: &str) -> Value {
    let started = Instant::now();
    loop {
        let described = described(emulator, case, name).await;
        let status = text(&described, "TableStatus");
        if status == "ACTIVE" {
            return described;
        }
        assert!(
            started.elapsed() < SETTLED_BUDGET,
            "case {case:?}: expected table {name:?} to reach ACTIVE within {SETTLED_BUDGET:?}, \
             it is still {status:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// The resource model `GetResource` reports for `reference`, required to be answered, with
/// the identifier the resource answers to.
async fn resolved(emulator: &Emulator, case: &str, reference: &str) -> (String, Value) {
    let (status, body) = emulator.get(TABLE, reference).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResource expected HTTP 200, got {status} with body {body:?}"
    );
    let description = &body["ResourceDescription"];
    (text(description, "Identifier"), properties_of(description))
}

/// The identifiers `ListResources` reports for the tables, walking every page.
async fn inventory(emulator: &Emulator) -> Vec<String> {
    let listed = emulator.list(TABLE, None, None).await;
    descriptions(&listed)
        .iter()
        .map(|description| text(description, "Identifier"))
        .collect()
}

/// `model` without the one property no case can spell: the ARN of the table's stream, which
/// the DynamoDB Streams service mints per stream. A row that enables a stream pins it against
/// what the OWNING service reports instead.
fn without_stream_arn(model: &Value) -> Value {
    let mut carried: Map<String, Value> = model.as_object().cloned().unwrap_or_default();
    carried.remove("StreamArn");
    Value::Object(carried)
}

/// One row of the native table: the case, the table's name, the `CreateTable` request the
/// OWNING service's own API is called with, and the resource model Cloud Control API must
/// then report for it.
type NativeRow<'a> = (&'a str, &'a str, Value, Value);

/// The tables the native table creates, as `(name, CreateTable request, model)` rows.
///
/// The rows differ along the shape of the primary key, the capacity mode, the indexes and
/// stream carried, the storage class, the deletion protection and the tag set.
fn native_rows() -> [NativeRow<'static>; 4] {
    [
        (
            "a provisioned table with a partition key and tags",
            "native-orders",
            json!({
                "TableName": "native-orders",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 5,
                    "WriteCapacityUnits": 3,
                },
                "Tags": [{ "Key": "owner", "Value": "platform" }],
            }),
            json!({
                "TableName": "native-orders",
                "Arn": table_arn("native-orders"),
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 5,
                    "WriteCapacityUnits": 3,
                },
                "DeletionProtectionEnabled": false,
                "Tags": [{ "Key": "owner", "Value": "platform" }],
            }),
        ),
        (
            "an on-demand table with a composite primary key",
            "native-events",
            json!({
                "TableName": "native-events",
                "KeySchema": [
                    { "AttributeName": "id", "KeyType": "HASH" },
                    { "AttributeName": "at", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "id", "AttributeType": "S" },
                    { "AttributeName": "at", "AttributeType": "N" },
                ],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            json!({
                "TableName": "native-events",
                "Arn": table_arn("native-events"),
                "KeySchema": [
                    { "AttributeName": "id", "KeyType": "HASH" },
                    { "AttributeName": "at", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "id", "AttributeType": "S" },
                    { "AttributeName": "at", "AttributeType": "N" },
                ],
                "BillingMode": "PAY_PER_REQUEST",
                "DeletionProtectionEnabled": false,
            }),
        ),
        (
            "a table with a global secondary index and a stream",
            "native-indexed",
            json!({
                "TableName": "native-indexed",
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "owner", "AttributeType": "S" },
                ],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
                "GlobalSecondaryIndexes": [{
                    "IndexName": "by-owner",
                    "KeySchema": [{ "AttributeName": "owner", "KeyType": "HASH" }],
                    "Projection": { "ProjectionType": "ALL" },
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": 2,
                        "WriteCapacityUnits": 2,
                    },
                }],
                "StreamSpecification": {
                    "StreamEnabled": true,
                    "StreamViewType": "NEW_AND_OLD_IMAGES",
                },
            }),
            json!({
                "TableName": "native-indexed",
                "Arn": table_arn("native-indexed"),
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "owner", "AttributeType": "S" },
                ],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
                "DeletionProtectionEnabled": false,
                "GlobalSecondaryIndexes": [{
                    "IndexName": "by-owner",
                    "KeySchema": [{ "AttributeName": "owner", "KeyType": "HASH" }],
                    "Projection": { "ProjectionType": "ALL" },
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": 2,
                        "WriteCapacityUnits": 2,
                    },
                }],
                "StreamSpecification": { "StreamViewType": "NEW_AND_OLD_IMAGES" },
            }),
        ),
        (
            "a protected table with a local secondary index and the infrequent-access class",
            "native-archive",
            json!({
                "TableName": "native-archive",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "version", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "version", "AttributeType": "N" },
                    { "AttributeName": "title", "AttributeType": "S" },
                ],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
                "LocalSecondaryIndexes": [{
                    "IndexName": "by-title",
                    "KeySchema": [
                        { "AttributeName": "pk", "KeyType": "HASH" },
                        { "AttributeName": "title", "KeyType": "RANGE" },
                    ],
                    "Projection": { "ProjectionType": "KEYS_ONLY" },
                }],
                "TableClass": "STANDARD_INFREQUENT_ACCESS",
                "DeletionProtectionEnabled": true,
            }),
            json!({
                "TableName": "native-archive",
                "Arn": table_arn("native-archive"),
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "version", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "version", "AttributeType": "N" },
                    { "AttributeName": "title", "AttributeType": "S" },
                ],
                "BillingMode": "PROVISIONED",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
                "DeletionProtectionEnabled": true,
                "LocalSecondaryIndexes": [{
                    "IndexName": "by-title",
                    "KeySchema": [
                        { "AttributeName": "pk", "KeyType": "HASH" },
                        { "AttributeName": "title", "KeyType": "RANGE" },
                    ],
                    "Projection": { "ProjectionType": "KEYS_ONLY" },
                }],
                "TableClass": "STANDARD_INFREQUENT_ACCESS",
            }),
        ),
    ]
}

#[tokio::test]
async fn a_table_dynamodb_holds_is_resolved_and_listed_through_cloud_control() {
    let emulator = Emulator::with_dynamodb();
    for (case, name, create, expected) in native_rows() {
        // Created through the OWNING service's own API: nothing of this table is ever named
        // to Cloud Control API before the reads below.
        emulator.dynamodb_ok("CreateTable", create).await;
        let described = active(&emulator, case, name).await;

        let (identifier, model) = resolved(&emulator, case, name).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: expected the table's own name, got {identifier:?}"
        );
        assert_eq!(
            without_stream_arn(&model),
            expected,
            "case {case:?}: expected {expected:?}, got {model:?}"
        );
        // The ARN in the model is the one the OWNING service reports for the very same
        // table, so the two planes name one resource.
        assert_eq!(
            model["Arn"], described["TableArn"],
            "case {case:?}: expected the ARN the owning service reports ({:?}), got {:?}",
            described["TableArn"], model["Arn"]
        );
        // And so is the stream's, for a table that has one: the ARN is minted by the
        // streams registry, so the model carries it exactly where that service does.
        assert_eq!(
            model.get("StreamArn"),
            described.get("LatestStreamArn"),
            "case {case:?}: expected the stream ARN the owning service reports, got {model:?}"
        );
        assert!(
            inventory(&emulator).await.contains(&name.to_string()),
            "case {case:?}: the table must be in the type's inventory"
        );
    }

    // The whole inventory is the owning store's own, in the ascending name order its own
    // `ListTables` answers in — and it pages, one resource at a time, over exactly those
    // tables.
    let expected: Vec<String> = vec![
        "native-archive".to_string(),
        "native-events".to_string(),
        "native-indexed".to_string(),
        "native-orders".to_string(),
    ];
    for (case, page_size) in [("one page", None), ("one resource per page", Some(1))] {
        let walked = emulator.walk(TABLE, page_size).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the tables the owning service holds, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_table_deleted_in_either_plane_stops_resolving_in_both() {
    let emulator = Emulator::with_dynamodb();
    // The rows differ along WHICH API removed the table: the owning service's own, and Cloud
    // Control API. Either way the table is gone from the one store it lived in, so both
    // planes report it as absent.
    let rows = [
        ("removed through the owning service's own API", "doomed-native", false),
        ("removed through Cloud Control API", "doomed-by-cloud-control", true),
    ];
    for (case, name, through_cloud_control) in rows {
        emulator
            .dynamodb_ok(
                "CreateTable",
                json!({
                    "TableName": name,
                    "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                    "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                    "BillingMode": "PAY_PER_REQUEST",
                }),
            )
            .await;
        active(&emulator, case, name).await;
        let (identifier, _) = resolved(&emulator, case, name).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: the live table must resolve under its own name"
        );

        if through_cloud_control {
            emulator.delete_and_settle(TABLE, name).await;
        } else {
            emulator
                .dynamodb_ok("DeleteTable", json!({ "TableName": name }))
                .await;
            // The owning service's own delete leaves the table `DELETING` until the clock
            // owes it the removal, so the case waits for the condition it is about: the
            // table being gone from that service's own read.
            let started = Instant::now();
            loop {
                let (status, _) = emulator
                    .dynamodb("DescribeTable", json!({ "TableName": name }))
                    .await;
                if status != 200 {
                    break;
                }
                assert!(
                    started.elapsed() < SETTLED_BUDGET,
                    "case {case:?}: expected the owning service to remove {name:?} within \
                     {SETTLED_BUDGET:?}"
                );
                tokio::time::sleep(SETTLED_INTERVAL).await;
            }
        }

        // The owning service no longer holds it, in its own terms.
        let (status, body) = emulator
            .dynamodb("DescribeTable", json!({ "TableName": name }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: dynamodb:DescribeTable after the delete expected HTTP 400, got \
             {status} with body {body:?}"
        );
        assert!(
            code(&body).contains(TABLE_NOT_FOUND),
            "case {case:?}: expected {TABLE_NOT_FOUND}, got {:?}",
            code(&body)
        );

        // And Cloud Control API reports the handler's not-found verdict for it.
        let (status, body) = emulator.get(TABLE, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: GetResource after the delete expected HTTP 400, got {status} with \
             body {body:?}"
        );
        assert_handler_not_found(&message(&body), TABLE, &handler_not_found(name), case);
        assert!(
            !inventory(&emulator).await.contains(&name.to_string()),
            "case {case:?}: the deleted table must be listed nowhere"
        );
    }
}

/// One row of the create table: the case, the desired state the create carries, and the
/// table name the resource must answer to — `None` for a model that names no table, whose
/// name the service picks.
type CreateRow<'a> = (&'a str, Value, Option<&'a str>);

#[tokio::test]
async fn a_table_created_through_cloud_control_is_the_table_dynamodb_holds() {
    let emulator = Emulator::with_dynamodb();
    // The rows differ along the shape of the primary key, the capacity mode and capacity,
    // the settings carried, and whether the model names the table at all.
    let rows: [CreateRow; 3] = [
        (
            "a provisioned table with a composite key and tags",
            json!({
                "TableName": "provisioned-by-cloud-control",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "sk", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "sk", "AttributeType": "S" },
                ],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 4,
                    "WriteCapacityUnits": 2,
                },
                "Tags": [{ "Key": "team", "Value": "platform" }],
            }),
            Some("provisioned-by-cloud-control"),
        ),
        (
            "an on-demand table in the infrequent-access class",
            json!({
                "TableName": "on-demand-by-cloud-control",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
                "TableClass": "STANDARD_INFREQUENT_ACCESS",
            }),
            Some("on-demand-by-cloud-control"),
        ),
        (
            // "If you don't specify a name, CloudFormation generates a unique physical ID":
            // the schema declares `TableName` create-only but not required.
            "a model that names no table",
            json!({
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            None,
        ),
    ];

    for (case, desired, name) in rows {
        let identifier = emulator.provision(TABLE, &desired).await;
        if let Some(name) = name {
            assert_eq!(
                identifier, name,
                "case {case:?}: expected the table's own name, got {identifier:?}"
            );
        } else {
            assert!(
                !identifier.is_empty(),
                "case {case:?}: the service must name the table it created"
            );
        }

        // The service that OWNS the type holds the table and answers for it in its own
        // terms. A Cloud Control API request reports `SUCCESS` only once the type's handler
        // has stabilised the resource, so the table is ALREADY active rather than still
        // being created.
        let described = described(&emulator, case, &identifier).await;
        assert_eq!(
            text(&described, "TableStatus"),
            "ACTIVE",
            "case {case:?}: expected a table the owning service reports as active, got \
             {described:?}"
        );
        assert_eq!(
            described["KeySchema"], desired["KeySchema"],
            "case {case:?}: expected the primary key the create declared, got {described:?}"
        );
        assert_eq!(
            described["TableArn"],
            json!(table_arn(&identifier)),
            "case {case:?}: expected the ARN the owning service builds, got {described:?}"
        );

        // And it is writable through that service's own data plane, which is what a table
        // nothing provisioned would not be.
        let item: Map<String, Value> = desired["KeySchema"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|element| (text(element, "AttributeName"), json!({ "S": "written" })))
            .collect();
        emulator
            .dynamodb_ok(
                "PutItem",
                json!({ "TableName": identifier, "Item": Value::Object(item.clone()) }),
            )
            .await;
        let read = emulator
            .dynamodb_ok(
                "GetItem",
                json!({ "TableName": identifier, "Key": Value::Object(item.clone()) }),
            )
            .await;
        assert_eq!(
            read["Item"],
            Value::Object(item),
            "case {case:?}: the table must hold the item written to it, got {read:?}"
        );

        // And Cloud Control API reads back the model that service holds: every property the
        // desired state named, under the identifier the table answers to.
        let (_, model) = resolved(&emulator, case, &identifier).await;
        assert_eq!(
            model["TableName"],
            json!(identifier),
            "case {case:?}: expected the model to name the table, got {model:?}"
        );
        assert_eq!(
            model["Arn"],
            json!(table_arn(&identifier)),
            "case {case:?}: expected the ARN the owning service builds, got {model:?}"
        );
        for property in ["KeySchema", "AttributeDefinitions", "TableClass"] {
            if let Some(wanted) = desired.get(property) {
                assert_eq!(
                    &model[property], wanted,
                    "case {case:?} ({property}): expected {wanted:?}, got {:?}",
                    model[property]
                );
            }
        }
        let expected_mode = desired
            .get("BillingMode")
            .cloned()
            .unwrap_or_else(|| json!("PROVISIONED"));
        assert_eq!(
            model["BillingMode"], expected_mode,
            "case {case:?}: expected the capacity mode the create resolved, got {model:?}"
        );
        assert_eq!(
            model.get("ProvisionedThroughput"),
            desired.get("ProvisionedThroughput"),
            "case {case:?}: expected the capacity the create declared, got {model:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_through_cloud_control_changes_the_table_dynamodb_holds() {
    let emulator = Emulator::with_dynamodb();
    let name = "patched-by-cloud-control";
    let identifier = emulator
        .provision(
            TABLE,
            &json!({
                "TableName": name,
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
            }),
        )
        .await;

    // Each patch of a property this path applies is a WRITE to the table the owning service
    // holds. The rows differ along the property patched, what the model then reports and what
    // the OWNING service then holds.
    let rows = [
        (
            "the provisioned capacity",
            json!([
                { "op": "replace", "path": "/ProvisionedThroughput/ReadCapacityUnits", "value": 7 },
                { "op": "replace", "path": "/ProvisionedThroughput/WriteCapacityUnits", "value": 5 },
            ]),
            vec![(
                "ProvisionedThroughput",
                json!({ "ReadCapacityUnits": 7, "WriteCapacityUnits": 5 }),
            )],
        ),
        (
            "the storage class and the deletion protection",
            json!([
                { "op": "add", "path": "/TableClass", "value": "STANDARD_INFREQUENT_ACCESS" },
                { "op": "replace", "path": "/DeletionProtectionEnabled", "value": true },
            ]),
            vec![
                ("TableClass", json!("STANDARD_INFREQUENT_ACCESS")),
                ("DeletionProtectionEnabled", json!(true)),
            ],
        ),
    ];
    for (case, patch, expected) in rows {
        emulator.update_and_settle(TABLE, &identifier, &patch).await;
        // The resource the patch left behind, as a read of it reports: the model is the one
        // the owning service holds, so each patched property reads back from THERE.
        let model = emulator.properties(TABLE, &identifier).await;
        for (property, wanted) in expected {
            assert_eq!(
                model[property], wanted,
                "case {case:?} ({property}): expected {wanted:?}, got {:?}",
                model[property]
            );
        }
        // And the table is settled once the request reports success: a Cloud Control
        // operation completes only when the type's handler has stabilised the resource.
        let settled = described(&emulator, case, name).await;
        assert_eq!(
            text(&settled, "TableStatus"),
            "ACTIVE",
            "case {case:?}: expected the owning service to report a settled table, got \
             {settled:?}"
        );
    }

    let described = described(&emulator, "after the patches", name).await;
    let throughput = &described["ProvisionedThroughput"];
    assert_eq!(
        (
            throughput["ReadCapacityUnits"].clone(),
            throughput["WriteCapacityUnits"].clone()
        ),
        (json!(7), json!(5)),
        "the owning service must hold the capacity the patch asked for, got {described:?}"
    );
    assert_eq!(
        described["TableClassSummary"]["TableClass"],
        json!("STANDARD_INFREQUENT_ACCESS"),
        "the owning service must hold the storage class the patch asked for, got {described:?}"
    );
    assert_eq!(
        described["DeletionProtectionEnabled"],
        json!(true),
        "the owning service must hold the deletion protection the patch asked for, got \
         {described:?}"
    );
    // The protection the patch set is in force on that service's OWN delete, which is what
    // makes it a setting of the table rather than a value this service echoes.
    let (status, body) = emulator
        .dynamodb("DeleteTable", json!({ "TableName": name }))
        .await;
    assert_eq!(
        status, 400,
        "the owning service must refuse to delete a protected table, got {status} with body \
         {body:?}"
    );

    // A patch asking for a property of the type no handler here applies is not applied at
    // all: the request reaches the owning handler's verdict, and the table keeps the model
    // it had.
    let before = emulator.properties(TABLE, &identifier).await;
    let rows = [
        (
            "server-side encryption",
            "SSESpecification",
            json!({ "SSEEnabled": true }),
        ),
        (
            "continuous backups",
            "PointInTimeRecoverySpecification",
            json!({ "PointInTimeRecoveryEnabled": true }),
        ),
    ];
    for (case, property, value) in rows {
        let (status, accepted) = emulator
            .update(
                TABLE,
                &identifier,
                &json!([{ "op": "add", "path": format!("/{property}"), "value": value }]),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the request to be accepted, got {status} with body \
             {accepted:?}"
        );
        let event = emulator
            .terminal(&text(&progress(&accepted), "RequestToken"))
            .await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {event:?}"
        );
        assert_eq!(
            text(&event, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the owning handler's verdict, got {event:?}"
        );
        assert_eq!(
            text(&event, "StatusMessage"),
            format!("{property} is a property of {TABLE} this emulator's handler does not apply"),
            "case {case:?}: unexpected wording, got {event:?}"
        );
        let after = emulator.properties(TABLE, &identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: the refused patch must leave the table as it was, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_create_the_owning_service_would_refuse_fails_with_the_handlers_verdict() {
    let emulator = Emulator::with_dynamodb();
    // The rows differ along WHICH rule the desired state breaks: the owning service's own
    // rules for a declared table (a key attribute nothing defines, an attribute no key uses,
    // a capacity stated for an on-demand table, a name outside the form a table name takes),
    // a property this handler does not apply, and a property the type does not declare at
    // all. Each expected sentence is the OWNING service's own, which is what a client of
    // this API reads.
    let rows: [(&str, &str, Value, String); 6] = [
        (
            "a key attribute no AttributeDefinitions entry declares",
            "refused-undefined-key",
            json!({
                "TableName": "refused-undefined-key",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "other", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            // The owning service states its parameter-value sentence over the two lists the
            // caller has to reconcile: the whole key schema at fault, and every attribute
            // name the declaration defines.
            "One or more parameter values were invalid: Some index key attributes are not \
             defined in AttributeDefinitions. Keys: [id], AttributeDefinitions: [other]"
                .to_string(),
        ),
        (
            "an attribute no key schema uses",
            "refused-unused-definition",
            json!({
                "TableName": "refused-unused-definition",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [
                    { "AttributeName": "id", "AttributeType": "S" },
                    { "AttributeName": "spare", "AttributeType": "S" },
                ],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            "Some AttributeDefinitions are not used in any KeySchema: spare".to_string(),
        ),
        (
            "a capacity stated for an on-demand table",
            "refused-capacity",
            json!({
                "TableName": "refused-capacity",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
            }),
            "One or more parameter values were invalid: Neither ReadCapacityUnits nor \
             WriteCapacityUnits can be specified when BillingMode is PAY_PER_REQUEST"
                .to_string(),
        ),
        (
            "a name shorter than a table name may be",
            "ab",
            json!({
                "TableName": "ab",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            "1 validation error detected: Value 'ab' at 'tableName' failed to satisfy \
             constraint: Member must have length greater than or equal to 3"
                .to_string(),
        ),
        (
            "a property of the type this handler does not apply",
            "refused-encrypted",
            json!({
                "TableName": "refused-encrypted",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
                "SSESpecification": { "SSEEnabled": true },
            }),
            format!(
                "SSESpecification is a property of {TABLE} this emulator's handler does not \
                 apply"
            ),
        ),
        (
            "a property the type does not declare",
            "refused-stranger",
            json!({
                "TableName": "refused-stranger",
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
                "Nope": "1",
            }),
            format!("Nope is not a property of {TABLE}"),
        ),
    ];

    for (case, name, desired, expected_message) in rows {
        let (status, body) = emulator.create(TABLE, &desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be accepted, got {status} with body {body:?}"
        );
        let event = emulator
            .terminal(&text(&progress(&body), "RequestToken"))
            .await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {event:?}"
        );
        assert_eq!(
            text(&event, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the owning handler's verdict, got {event:?}"
        );
        assert_eq!(
            text(&event, "StatusMessage"),
            expected_message,
            "case {case:?}: unexpected wording, got {event:?}"
        );
        // Nothing was written, in either plane: a refused create leaves no table behind.
        let (status, body) = emulator
            .dynamodb("DescribeTable", json!({ "TableName": name }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the refused create must leave no table, got {status} with body \
             {body:?}"
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
async fn a_table_class_patch_past_the_owning_quota_fails_with_the_handlers_verdict() {
    // RULE: a class this path MOVES is one of that table's own changes, so it is held to the
    // owning service's table-class change quota — twice in a 30-day trailing period — and a
    // patch past it fails with that service's capacity verdict instead of spending a change
    // the DynamoDB control plane itself refuses.
    let emulator = Emulator::with_dynamodb();
    let name = "class-quota-by-cloud-control";
    let identifier = emulator
        .provision(
            TABLE,
            &json!({
                "TableName": name,
                "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 1,
                    "WriteCapacityUnits": 1,
                },
            }),
        )
        .await;

    // The changes the quota admits, each alternating the class so every one of them moves it.
    for class in ["STANDARD_INFREQUENT_ACCESS", "STANDARD"] {
        emulator
            .update_and_settle(
                TABLE,
                &identifier,
                &json!([{ "op": "add", "path": "/TableClass", "value": class }]),
            )
            .await;
        let model = emulator.properties(TABLE, &identifier).await;
        assert_eq!(
            model["TableClass"],
            json!(class),
            "case a change inside the quota: expected {class:?}, got {model:?}"
        );
    }

    let before = emulator.properties(TABLE, &identifier).await;
    let (status, accepted) = emulator
        .update(
            TABLE,
            &identifier,
            &json!([{ "op": "add", "path": "/TableClass",
                      "value": "STANDARD_INFREQUENT_ACCESS" }]),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case the change past the quota: the request itself is accepted, got {status} with body \
         {accepted:?}"
    );
    let event = emulator
        .terminal(&text(&progress(&accepted), "RequestToken"))
        .await;
    assert_eq!(
        (
            text(&event, "OperationStatus"),
            text(&event, "ErrorCode"),
            text(&event, "StatusMessage"),
        ),
        (
            STATUS_FAILED.to_string(),
            "ServiceLimitExceeded".to_string(),
            "Subscriber limit exceeded: Updates to TableClass are limited to 2 times in 30 \
             day(s)."
                .to_string(),
        ),
        "case the change past the quota: expected the owning handler's capacity verdict, got \
         {event:?}"
    );
    // Nothing was written, in either plane: the table keeps the class the last admitted
    // change left it in.
    let after = emulator.properties(TABLE, &identifier).await;
    let described = described(&emulator, "after the refused patch", name).await;
    assert_eq!(
        (after, described["TableClassSummary"]["TableClass"].clone()),
        (before, json!("STANDARD")),
        "case the change past the quota: the refused patch left the class as it was, got \
         {described:?}"
    );
}
