// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation update that FAILS and rolls back is not reported terminal before the
//! service that owns a resource the rollback removed has stopped holding it — so the first
//! call a client makes after `UPDATE_ROLLBACK_COMPLETE` finds the added table gone.
//!
//! The rule under test: **a stack operation reaches a terminal status only once every
//! resource it deleted has really been taken away by the service that owns it; a resource
//! whose delete that service only ACCEPTED holds the operation open, and once it is gone the
//! owning service refuses every read of it.** For a DynamoDB table the delete is
//! asynchronous — "DynamoDB immediately returns a response with a `TableStatus` of
//! `DELETING`", and only once the table has been deleted does the name stop resolving, at
//! which point `DescribeTable` answers `ResourceNotFoundException` (HTTP 400) with
//! "Requested resource not found: Table: <TableName> not found" for the name the request
//! carried. A rollback reported complete while the table it removed were still `DELETING`
//! would leave `DescribeTable` answering 200 for a table CloudFormation says is gone, so any
//! client that waits for the removed resource to disappear would wait forever. Two crates
//! have to agree for that to hold (CloudFormation makes the calls; DynamoDB decides when the
//! table stops resolving), so the suite lives here rather than in either one.
//!
//! The rows differ along what the rule generalises over: how many tables the failed update
//! added (so the rollback waits for each of them, not just the first), the key schema and
//! billing mode the added table was created with, and whether the template names the table
//! or leaves the name to CloudFormation. The recorded case is one row of it.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery CloudFormation caller, the poll that waits for a stack to FIRST report a
// status, the awsJson DynamoDB caller and the XML readers are each written ONCE in this test
// crate: a suite that copies one drifts from the others.
use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_dynamodb_table_in_place_update::described_table;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, members_of};

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The logical id of the table every row's stack starts with and keeps.
const KEPT: &str = "Kept";

/// The logical id of the resource whose declaration the update breaks, which is what fails
/// the update and so forces the rollback.
const BROKEN: &str = "Broken";

/// The partition key every well-formed table below is declared with.
const KEY: &str = "pk";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation makes the calls, DynamoDB decides
/// whether the table still resolves.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// A well-formed `AWS::DynamoDB::Table` declaration, carrying a `TableName` only where the
/// row asks for one, with `extra` merged over the on-demand hash-keyed default.
fn table(name: Option<&str>, extra: Value) -> Value {
    let mut properties = json!({
        "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    });
    let object = properties.as_object_mut().expect("an object");
    if let Some(name) = name {
        object.insert("TableName".to_string(), json!(name));
    }
    for (member, value) in extra.as_object().expect("an object") {
        object.insert(member.clone(), value.clone());
    }
    json!({"Type": "AWS::DynamoDB::Table", "Properties": properties})
}

/// A table declaration the deployment cannot carry out: its `KeySchema` names an attribute
/// that `AttributeDefinitions` does not declare. WHAT fails the update is immaterial to the
/// rule — only that the update fails — and this is the recorded case's own defect.
fn broken_table() -> Value {
    json!({
        "Type": "AWS::DynamoDB::Table",
        "Properties": {
            "KeySchema": [{"AttributeName": "sk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        },
    })
}

/// A template over the resources `declared` names.
fn template(declared: Vec<(String, Value)>) -> String {
    let mut resources = serde_json::Map::new();
    for (logical_id, declaration) in declared {
        resources.insert(logical_id, declaration);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// One table the failed update ADDS: its logical id, the `TableName` its declaration carries
/// (`None` leaves the name to CloudFormation) and the properties that make it this row's
/// shape.
struct Added {
    logical_id: &'static str,
    declared_name: Option<&'static str>,
    properties: Value,
}

/// One row: the tables one failed update adds beside the table the stack keeps.
struct Row {
    case: &'static str,
    added: Vec<Added>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: one on-demand table added by the update.
            case: "one on-demand table added",
            added: vec![Added {
                logical_id: "Added",
                declared_name: Some("rollback-removes-one"),
                properties: json!({}),
            }],
        },
        Row {
            case: "two tables added at once",
            added: vec![
                Added {
                    logical_id: "AddedFirst",
                    declared_name: Some("rollback-removes-first"),
                    properties: json!({}),
                },
                Added {
                    logical_id: "AddedSecond",
                    declared_name: Some("rollback-removes-second"),
                    properties: json!({}),
                },
            ],
        },
        Row {
            // A different schema and billing mode: the rule is about the removal, not about
            // what the table was configured with.
            case: "a provisioned table with a range key",
            added: vec![Added {
                logical_id: "Added",
                declared_name: Some("rollback-removes-a-composite-key"),
                properties: json!({
                    "BillingMode": "PROVISIONED",
                    "ProvisionedThroughput": {
                        "ReadCapacityUnits": 2,
                        "WriteCapacityUnits": 3,
                    },
                    "KeySchema": [
                        {"AttributeName": KEY, "KeyType": "HASH"},
                        {"AttributeName": "sk", "KeyType": "RANGE"},
                    ],
                    "AttributeDefinitions": [
                        {"AttributeName": KEY, "AttributeType": "S"},
                        {"AttributeName": "sk", "AttributeType": "N"},
                    ],
                }),
            }],
        },
        Row {
            // A table CloudFormation names: the identity the rollback removed is the one the
            // stack published for it, not a name this suite chose.
            case: "a table CloudFormation names",
            added: vec![Added {
                logical_id: "Added",
                declared_name: None,
                properties: json!({}),
            }],
        },
    ]
}

/// Every row: the update that adds the tables fails, the stack reaches
/// `UPDATE_ROLLBACK_COMPLETE`, and at that FIRST observation DynamoDB refuses every table the
/// rollback removed — while the table the stack kept is still served.
#[tokio::test]
async fn an_update_rollback_is_terminal_only_once_the_added_tables_are_gone() {
    for (index, row) in rows().into_iter().enumerate() {
        let router = build_router();
        let case = row.case;
        let stack_name = format!("rollback-removes-added-table-{index}");
        let kept_name = format!("rollback-keeps-{index}");

        // ---- The stack the failed update is deployed against. ----
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                (
                    "TemplateBody",
                    &template(vec![(
                        KEPT.to_string(),
                        table(Some(&kept_name), json!({})),
                    )]),
                ),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (stack_status, described) =
            wait_for_stack_status(&router, &stack_name, CREATE_COMPLETE).await;
        assert_eq!(
            stack_status, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE} before the update, got \
             {described}"
        );

        // ---- The update that adds the tables and fails. ----
        let mut declared: Vec<(String, Value)> =
            vec![(KEPT.to_string(), table(Some(&kept_name), json!({})))];
        for added in &row.added {
            declared.push((
                added.logical_id.to_string(),
                table(added.declared_name, added.properties.clone()),
            ));
        }
        declared.push((BROKEN.to_string(), broken_table()));
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template(declared)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );

        // ---- The terminal rollback, and what DynamoDB answers at that instant. ----
        let (stack_status, described) =
            wait_for_stack_status(&router, &stack_name, UPDATE_ROLLBACK_COMPLETE).await;
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", &stack_name)]).await;
        assert_eq!(
            stack_status,
            UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to reach {UPDATE_ROLLBACK_COMPLETE}, got \
             {described} with resource failures {:?}",
            elements(&events, "ResourceStatusReason")
        );

        for added in &row.added {
            // The name the added table was really created under: what the stack itself
            // published for it, so a row that named no table is held to the name
            // CloudFormation chose rather than one this suite invented.
            let name = published_name(&events, added.logical_id).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the stack published no table name for {:?} in {events}",
                    added.logical_id
                )
            });
            if let Some(declared) = added.declared_name {
                assert_eq!(
                    name, declared,
                    "case {case:?}: expected {:?} to be created under the declared name \
                     {declared:?}, got {name:?}",
                    added.logical_id
                );
            }
            let (status, answered) = described_table(&router, &name).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: the rollback removed {name:?}, so DescribeTable must refuse it \
                 the instant {UPDATE_ROLLBACK_COMPLETE} is first observed, got {status} with \
                 {answered}"
            );
            assert_eq!(
                error_message(&answered),
                format!("Requested resource not found: Table: {name} not found"),
                "case {case:?}: expected the not-found to name the table the request carried, \
                 got {answered}"
            );
        }

        // The table the stack KEPT is untouched: the rollback removed what the failed update
        // added, nothing else.
        let (status, kept) = described_table(&router, &kept_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the kept table {kept_name:?} to still resolve after the \
             rollback, got {status} with {kept}"
        );
        assert_eq!(
            kept["Table"]["TableStatus"],
            json!("ACTIVE"),
            "case {case:?}: expected the kept table to be ACTIVE after the rollback, got {kept}"
        );
    }
}

/// The physical id a `DescribeStackEvents` answer reports for `logical_id` — the identifier
/// the owning service really assigned the resource.
///
/// Read off the `<member>` that carries BOTH halves, through this crate's shared readers, and
/// taking the first NON-EMPTY one: the events of one resource span its whole lifecycle, and
/// the transition that OPENS a create is recorded before any identifier exists for it.
fn published_name(events: &str, logical_id: &str) -> Option<String> {
    members_of(events).into_iter().find_map(|member| {
        let reported = elements(member, "LogicalResourceId").into_iter().next()?;
        let physical = elements(member, "PhysicalResourceId").into_iter().next()?;
        (reported == logical_id && !physical.is_empty()).then_some(physical)
    })
}

/// The `__type` of an awsJson error, reduced to the error code the wire names it by.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an awsJson error.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}
