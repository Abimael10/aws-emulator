// SPDX-License-Identifier: CC0-1.0

//! WHICH properties document a stack event reports for a resource a deployment acted on, end
//! to end over the awsQuery wire.
//!
//! RULE under test: **`StackEvent$ResourceProperties` / `OperationEvent$ResourceProperties`
//! is "BLOB of the properties used to create the resource" — the declaration THAT transition
//! applied. So every event of a change in flight carries the document the deployment
//! submitted to the owning service, including the `*_FAILED` event of a change that service
//! REFUSED (nothing was applied, and what the event is about is what was asked for), while
//! the events of the revert that puts such a resource back carry the configuration being put
//! back.**
//!
//! Why it matters: the event stream is what a client reads to diagnose a failed deployment.
//! An event that reports the configuration the resource had before the update, on the very
//! transition that failed, tells the reader the deployment submitted something it did not —
//! so the one record of what was attempted is wrong exactly where it is needed.
//!
//! The two readers of that one log — `DescribeStackEvents` for a stack and `DescribeEvents`
//! for the operations that caused its events — are both asserted here, so neither can drift
//! from the other.
//!
//! The rows span the class the rule generalises over: a change whose refusal comes from a
//! bounded parameter's range, one from an enumerated parameter's value list, and one the
//! owning service APPLIES — over two resource types and two owning services, within one
//! update (a refused resource fails the update, so every resource it changed is put back).
//! Amazon RDS's own refusal of a `AWS::RDS::DBParameterGroup` value is the refusal used,
//! exactly as `update_rollback_of_a_refused_update.rs` drives it.

use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::settled_stack_status;
use crate::stack_lifecycle::{call, create_stack, member, router, xml_blocks, xml_text, REGION};

/// The stack the rows deploy, and the engine family its parameter groups are created in.
const STACK: &str = "event-properties";
const FAMILY: &str = "mysql8.0";

/// One row: a resource of the deployed template, the declaration the update states for it,
/// and the two documents its events have to report.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    /// The resource as the stack is CREATED with it, and as the update revises it.
    created: Value,
    updated: Value,
    /// Those two `Properties` sections as the event's blob spells them: the JSON document in
    /// KEY order, which is the spelling the service writes so the text cannot depend on how
    /// the workspace was built.
    created_blob: &'static str,
    updated_blob: &'static str,
    /// Whether the owning service REFUSES the revised declaration.
    refused: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a change refused because the value is outside the parameter's range",
            logical_id: "Ranged",
            created: parameter_group("properties-ranged", "max_connections", "150"),
            // Past the upper bound of the range that parameter publishes.
            updated: parameter_group("properties-ranged", "max_connections", "200000"),
            created_blob: r#"{"DBParameterGroupName":"properties-ranged","Description":"the event-properties probe","Family":"mysql8.0","Parameters":{"max_connections":"150"}}"#,
            updated_blob: r#"{"DBParameterGroupName":"properties-ranged","Description":"the event-properties probe","Family":"mysql8.0","Parameters":{"max_connections":"200000"}}"#,
            refused: true,
        },
        Row {
            case: "a change refused because the value is outside the parameter's value list",
            logical_id: "Listed",
            created: parameter_group(
                "properties-listed",
                "innodb_flush_log_at_trx_commit",
                "1",
            ),
            // Outside the value list that parameter publishes.
            updated: parameter_group(
                "properties-listed",
                "innodb_flush_log_at_trx_commit",
                "3",
            ),
            created_blob: r#"{"DBParameterGroupName":"properties-listed","Description":"the event-properties probe","Family":"mysql8.0","Parameters":{"innodb_flush_log_at_trx_commit":"1"}}"#,
            updated_blob: r#"{"DBParameterGroupName":"properties-listed","Description":"the event-properties probe","Family":"mysql8.0","Parameters":{"innodb_flush_log_at_trx_commit":"3"}}"#,
            refused: true,
        },
        Row {
            case: "a change the owning service applies",
            logical_id: "Applied",
            created: json!({
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": "/event-properties/applied",
                    "Type": "String",
                    "Value": "before",
                },
            }),
            updated: json!({
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": "/event-properties/applied",
                    "Type": "String",
                    "Value": "after",
                },
            }),
            created_blob: r#"{"Name":"/event-properties/applied","Type":"String","Value":"before"}"#,
            updated_blob: r#"{"Name":"/event-properties/applied","Type":"String","Value":"after"}"#,
            refused: false,
        },
    ]
}

/// One `AWS::RDS::DBParameterGroup` declaration: the group Amazon RDS really holds, carrying
/// one value for one of the parameters of its family.
fn parameter_group(name: &str, parameter: &str, value: &str) -> Value {
    json!({
        "Type": "AWS::RDS::DBParameterGroup",
        "Properties": {
            "Description": "the event-properties probe",
            "Family": FAMILY,
            "DBParameterGroupName": name,
            "Parameters": {parameter: value},
        },
    })
}

/// The template the stack is deployed with: every row's declaration, created or updated.
fn template(rows: &[Row], updated: bool) -> String {
    let mut resources = Map::new();
    for row in rows {
        let declaration = if updated {
            row.updated.clone()
        } else {
            row.created.clone()
        };
        resources.insert(row.logical_id.to_string(), declaration);
    }
    json!({"Resources": Value::Object(resources)}).to_string()
}

/// One event, in the three members these rows read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Event {
    logical_id: String,
    resource_status: String,
    resource_properties: Option<String>,
}

/// Every event of one member list, read INSIDE its own `<member>` blocks so a nested list
/// cannot be swept up with it.
fn events_of(body: &str) -> Vec<Event> {
    xml_blocks(body, "member")
        .iter()
        .map(|entry| Event {
            logical_id: xml_text(entry, "LogicalResourceId").unwrap_or_default(),
            resource_status: xml_text(entry, "ResourceStatus").unwrap_or_default(),
            resource_properties: xml_text(entry, "ResourceProperties"),
        })
        .collect()
}

/// The stack's own event history, newest first.
async fn stack_events(case: &str, router: &MultiServiceRouter) -> Vec<Event> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", STACK)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents must answer 200, got {code} {body}"
    );
    events_of(&body)
}

/// The same log read as the events of the OPERATIONS that caused them.
async fn operation_events(case: &str, router: &MultiServiceRouter) -> Vec<Event> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", STACK)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeEvents must answer 200, got {code} {body}"
    );
    events_of(&body)
}

/// The properties documents the events of one resource under one status report.
fn reported(events: &[Event], logical_id: &str, resource_status: &str) -> Vec<Option<String>> {
    events
        .iter()
        .filter(|event| event.logical_id == logical_id && event.resource_status == resource_status)
        .map(|event| event.resource_properties.clone())
        .collect()
}

#[tokio::test]
async fn an_event_reports_the_properties_its_own_transition_applied() {
    let case_rows = rows();
    let router = router();
    let (code, body) = create_stack(&router, STACK, &template(&case_rows, false), &[]).await;
    assert_eq!(code, 200, "CreateStack must answer 200, got {code} {body}");
    let settled = settled_stack_status("the create", &router, REGION, STACK).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "expected the create to complete before the update, got {settled:?}"
    );

    // Every resource's create events report the declaration the create applied, which is the
    // same rule read on the transition where nothing was in force before.
    let created = stack_events("the create", &router).await;
    for row in &case_rows {
        let blobs = reported(&created, row.logical_id, resource_status::CREATE_COMPLETE);
        assert_eq!(
            blobs,
            vec![Some(row.created_blob.to_string())],
            "case {:?}: expected the create's own event to report the declaration it \
             applied, got {blobs:?}",
            row.case
        );
    }

    // The update submits every row's revised declaration. Two of them the owning service
    // refuses, so the update fails and is rolled back — and every resource it had changed is
    // put back.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", STACK),
            member("TemplateBody", &template(&case_rows, true)),
        ],
    )
    .await;
    assert_eq!(code, 200, "UpdateStack must answer 200, got {code} {body}");
    let settled = settled_stack_status("the update", &router, REGION, STACK).await;
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "expected the refused update to be rolled back, got {settled:?}"
    );

    for events in [
        stack_events("the update", &router).await,
        operation_events("the update", &router).await,
    ] {
        for row in &case_rows {
            // The change in flight: the transition that OPENED it reports the document the
            // deployment submitted. The revert of the same resource opens a transition of its
            // own, which reports the configuration IT applies, so both documents are due here
            // and neither may be the other.
            let opened = reported(&events, row.logical_id, resource_status::UPDATE_IN_PROGRESS);
            assert!(
                opened.contains(&Some(row.updated_blob.to_string())),
                "case {:?}: expected the submitted declaration on the change's own \
                 transition, got {opened:?}",
                row.case
            );
            assert!(
                opened.contains(&Some(row.created_blob.to_string())),
                "case {:?}: expected the restored declaration on the revert's own \
                 transition, got {opened:?}",
                row.case
            );
            // ...and the revert ENDED with the configuration it put back in force.
            let completed = reported(&events, row.logical_id, resource_status::UPDATE_COMPLETE);
            assert!(
                completed.contains(&Some(row.created_blob.to_string())),
                "case {:?}: expected the revert's completion to report the restored \
                 declaration, got {completed:?}",
                row.case
            );
            // The refusal itself is about what was ASKED FOR: the resource the owning
            // service refused carries the submitted document on its failed transition, never
            // the one that stayed in force.
            let failed = reported(&events, row.logical_id, resource_status::UPDATE_FAILED);
            if row.refused {
                assert!(
                    !failed.is_empty()
                        && failed
                            .iter()
                            .all(|blob| blob.as_deref() == Some(row.updated_blob)),
                    "case {:?}: expected every failed transition to report the submitted \
                     declaration, got {failed:?}",
                    row.case
                );
            } else {
                assert!(
                    failed.is_empty(),
                    "case {:?}: expected no failed transition for a change the owning \
                     service applied, got {failed:?}",
                    row.case
                );
            }
        }
    }
}
