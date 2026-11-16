// SPDX-License-Identifier: CC0-1.0

//! The deployment an in-use export CANCELS, and what its stack reports having done, end to
//! end over the awsQuery wire.
//!
//! RULE under test: **the exports a deployment publishes are installed when that deployment
//! COMPLETES, so the two rules about an exported output value are decided at that stage — a
//! name another live stack already publishes, and a value another live stack still IMPORTS.
//! The deployment is therefore CARRIED OUT and then cancelled: the resources it changed in
//! the services that own them are changed, the operation fails naming the export and the
//! stack that imports it, and the ROLLBACK that failure owes puts those resources back —
//! which is why the stack's own event history reports the resource transitions of the update
//! AND of its revert beside the stack's `UPDATE_ROLLBACK_IN_PROGRESS`,
//! `UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS` and `UPDATE_ROLLBACK_COMPLETE`.**
//!
//! Why it matters: the event history is what a client polling a deployment reads to see what
//! happened to it. A cancellation that reports only the stack's own statuses hides that the
//! update had already changed resources and that they were put back, and a client diagnosing
//! a blocked cross-stack change cannot tell a deployment that was undone from one that never
//! ran.
//!
//! `update_stack_export_in_use.rs` pins WHICH deployments the pin cancels (which export is
//! withdrawn, how, and by which call) and `export_pin_release.rs` pins the release of the pin.
//! The rows here differ along what this rule generalises over: whether the cancelled
//! deployment also CHANGES a resource in an owning service (and so has something to put
//! back), how the export is withdrawn (its value changes, or it is dropped), and which call
//! deploys the revision (`UpdateStack`, `ExecuteChangeSet`). The last row is the control: the
//! same resource change with the in-use export republished unchanged goes through.

use serde_json::json;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::settled_stack_status;
use crate::stack_lifecycle::{
    call, create_stack, member, router, wait_for_the_transition, xml_blocks, xml_text, xml_texts,
    REGION,
};
use crate::update_rollback_recovery::parameter_value;

/// The stack that publishes the export, and the one that imports it.
const EXPORTER: &str = "canceled-exporter";
const IMPORTER: &str = "canceled-importer";

/// The export name the rows pin, and the value the importer resolved against.
const EXPORT: &str = "canceled-shared-export";
const PUBLISHED: &str = "published-first";

/// The `StackStatusReason` CloudFormation records on a deployment it cancelled because the
/// deployment would have withdrawn an exported output value another stack still imports.
///
/// Spelled out here rather than read off the emulator's own formatter, so the assertion pins
/// the wording: fixed text around the export name and the importing stack's name, ending with
/// its period.
fn update_canceled_reason(export_name: &str, importing_stack_name: &str) -> String {
    format!(
        "Update canceled. Cannot update export {export_name} as it is in use by \
         {importing_stack_name}."
    )
}

/// Which call deploys the revision: both are deployments of the same stack, and the stage the
/// export rules are decided at is the deployment's, not the call's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    UpdateStack,
    ChangeSet,
}

/// One row: the revision the exporting stack is asked to deploy, and where it must end up.
struct Row {
    case: &'static str,
    /// The Parameter Store name the exporter's own resource writes, or `None` for a row whose
    /// revision changes no resource at all.
    parameter: Option<&'static str>,
    /// The value that parameter holds once the row has settled.
    settled_value: Option<&'static str>,
    /// What the revision publishes under [`EXPORT`], or `None` where it DROPS the export.
    revised_publication: Option<&'static str>,
    driver: Driver,
    /// Where the stack ends up, and the reason it carries there.
    settles_to: &'static str,
    cancelled: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the revision changes a resource and re-values the in-use export",
            parameter: Some("/canceled/changed"),
            // The cancellation's rollback puts the change back, so the value in force is the
            // one the update found.
            settled_value: Some("before"),
            revised_publication: Some("published-second"),
            driver: Driver::UpdateStack,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            cancelled: true,
        },
        Row {
            case: "the revision changes a resource and DROPS the in-use export",
            parameter: Some("/canceled/dropped"),
            settled_value: Some("before"),
            revised_publication: None,
            driver: Driver::UpdateStack,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            cancelled: true,
        },
        Row {
            case: "the revision is deployed by executing a change set",
            parameter: Some("/canceled/change-set"),
            settled_value: Some("before"),
            revised_publication: Some("published-by-the-change-set"),
            driver: Driver::ChangeSet,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            cancelled: true,
        },
        Row {
            case: "the revision re-values the in-use export and changes no resource",
            parameter: None,
            settled_value: None,
            revised_publication: Some("published-without-a-resource-change"),
            driver: Driver::UpdateStack,
            settles_to: status::UPDATE_ROLLBACK_COMPLETE,
            cancelled: true,
        },
        // The control: the same resource change with the in-use export republished unchanged
        // withdraws nothing, so the deployment is carried through.
        Row {
            case: "the revision changes a resource and republishes the export unchanged",
            parameter: Some("/canceled/control"),
            settled_value: Some("after"),
            revised_publication: Some(PUBLISHED),
            driver: Driver::UpdateStack,
            settles_to: status::UPDATE_COMPLETE,
            cancelled: false,
        },
    ]
}

/// The exporter's template: the row's resource (where it has one) and the output publishing
/// `publication` under [`EXPORT`] (where it publishes one).
fn exporter_template(row: &Row, value: &str, publication: Option<&str>) -> String {
    let mut resources = serde_json::Map::new();
    if let Some(name) = row.parameter {
        resources.insert(
            "Held".to_string(),
            json!({
                "Type": "AWS::SSM::Parameter",
                "Properties": {"Name": name, "Type": "String", "Value": value},
            }),
        );
    } else {
        resources.insert(
            "Handle".to_string(),
            json!({"Type": "AWS::CloudFormation::WaitConditionHandle"}),
        );
    }
    let mut outputs = serde_json::Map::new();
    match publication {
        Some(published) => {
            outputs.insert(
                "Exported".to_string(),
                json!({"Value": published, "Export": {"Name": EXPORT}}),
            );
        }
        // A revision that DROPS the export publishes an output of its own instead, so the
        // template still has an `Outputs` section and the only change is the export.
        None => {
            outputs.insert("Kept".to_string(), json!({"Value": "kept"}));
        }
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// The importing stack's template: it resolves [`EXPORT`] into an output of its own, which is
/// what puts the pin on the exporter.
fn importer_template() -> String {
    json!({
        "Resources": {"Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"}},
        "Outputs": {"Echoed": {"Value": {"Fn::ImportValue": EXPORT}}},
    })
    .to_string()
}

/// Every `(LogicalResourceId, ResourceStatus, ResourceStatusReason)` the stack's event
/// history carries, newest first.
async fn events(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(entry, "ResourceStatus").unwrap_or_default(),
                xml_text(entry, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// The value `ListExports` publishes under [`EXPORT`], or `None` where the Region publishes
/// no such export.
async fn published_value(case: &str, router: &MultiServiceRouter) -> Option<String> {
    let (code, body) = call(router, "ListExports", &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListExports must answer 200, got {code} {body}"
    );
    xml_texts(&body, "Name")
        .into_iter()
        .zip(xml_texts(&body, "Value"))
        .find(|(name, _)| name == EXPORT)
        .map(|(_, value)| value)
}

/// Deploy `template` onto the exporting stack by the call the row names, and assert the CALL
/// was accepted: the model declares no error for an export in use, so what the pin cancels is
/// the stack operation and never the request.
async fn deploy_the_revision(case: &str, router: &MultiServiceRouter, row: &Row, template: &str) {
    match row.driver {
        Driver::UpdateStack => {
            let (code, body) = call(
                router,
                "UpdateStack",
                &[
                    member("StackName", EXPORTER),
                    member("TemplateBody", template),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: UpdateStack must be accepted, got {code} {body}"
            );
        }
        Driver::ChangeSet => {
            let (code, body) = call(
                router,
                "CreateChangeSet",
                &[
                    member("StackName", EXPORTER),
                    member("ChangeSetName", "canceled-revision"),
                    member("ChangeSetType", "UPDATE"),
                    member("TemplateBody", template),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateChangeSet must be accepted, got {code} {body}"
            );
            let change_set = xml_text(&body, "Id").unwrap_or_default();
            wait_for_the_transition().await;
            let (code, body) = call(
                router,
                "ExecuteChangeSet",
                &[member("ChangeSetName", &change_set)],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: ExecuteChangeSet must be accepted, got {code} {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_deployment_an_in_use_export_cancels_is_carried_out_and_then_rolled_back() {
    for row in rows() {
        let case = row.case;
        let router = router();

        // The exporter publishes the value, and the importer resolves it: that is the state
        // the pin is derived from.
        let created = exporter_template(&row, "before", Some(PUBLISHED));
        let (code, body) = create_stack(&router, EXPORTER, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({EXPORTER}) must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, EXPORTER).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the exporter to be created, got {settled:?}"
        );
        let (code, body) = create_stack(&router, IMPORTER, &importer_template(), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({IMPORTER}) must be accepted, got {code} {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, IMPORTER).await;
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the importer to be created, got {settled:?}"
        );
        if let Some(name) = row.parameter {
            let held = parameter_value(&router, name).await;
            assert_eq!(
                held.as_deref(),
                Some("before"),
                "case {case:?}: expected Parameter Store to hold the created value, got {held:?}"
            );
        }

        // The revision is deployed, and where it ends up is what the rule decides.
        let revised = exporter_template(&row, "after", row.revised_publication);
        deploy_the_revision(case, &router, &row, &revised).await;
        let settled = settled_stack_status(case, &router, REGION, EXPORTER).await;
        assert_eq!(
            settled, row.settles_to,
            "case {case:?}: expected the deployment to settle at {}, got {settled:?}",
            row.settles_to
        );

        let reported = events(case, &router, EXPORTER).await;
        let expected_reason = update_canceled_reason(EXPORT, IMPORTER);
        if row.cancelled {
            // The cancellation is reported where a client polling the deployment reads it:
            // the stack's own rollback transitions, the reason on the one that OPENED the
            // rollback, and the documented cleanup phase before the terminal status.
            for expected in [
                (
                    status::UPDATE_ROLLBACK_IN_PROGRESS,
                    expected_reason.as_str(),
                ),
                (status::UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS, ""),
                (status::UPDATE_ROLLBACK_COMPLETE, ""),
            ] {
                assert!(
                    reported.iter().any(|(logical_id, reported_status, reason)| {
                        logical_id == EXPORTER
                            && reported_status == expected.0
                            && reason == expected.1
                    }),
                    "case {case:?}: expected the stack's own {} event carrying {:?}, got \
                     {reported:?}",
                    expected.0,
                    expected.1
                );
            }
        } else {
            assert!(
                !reported.iter().any(|(_, _, reason)| reason == &expected_reason),
                "case {case:?}: expected no cancellation of a deployment that withdraws \
                 nothing, got {reported:?}"
            );
        }

        // What the deployment DID to the resource it changed: it was changed, and — where the
        // export cancelled the deployment — put back, each transition on an event of the
        // resource's own.
        if let Some(name) = row.parameter {
            let of_resource: Vec<&(String, String, String)> = reported
                .iter()
                .filter(|(logical_id, _, _)| logical_id == "Held")
                .collect();
            let changed = of_resource
                .iter()
                .filter(|(_, reported_status, _)| {
                    reported_status == resource_status::UPDATE_IN_PROGRESS
                })
                .count();
            let expected_changes = if row.cancelled { 2 } else { 1 };
            assert_eq!(
                changed, expected_changes,
                "case {case:?}: expected the resource's change to be reported {expected_changes} \
                 time(s) (the update's, and the revert's where the deployment was cancelled), \
                 got {of_resource:?}"
            );
            assert!(
                of_resource.iter().any(|(_, reported_status, _)| {
                    reported_status == resource_status::UPDATE_COMPLETE
                }),
                "case {case:?}: expected the resource's own completed transition, got \
                 {of_resource:?}"
            );
            // ...and the owning service really holds the configuration that walk left: the
            // rollback is PERFORMED, not merely reported.
            let held = parameter_value(&router, name).await;
            assert_eq!(
                held.as_deref(),
                row.settled_value,
                "case {case:?}: expected Parameter Store to hold {:?}, got {held:?}",
                row.settled_value
            );
        }

        // ...and the export itself: a cancelled deployment goes on publishing what the
        // importer resolved, while the control row publishes what it was asked to.
        let publishing = published_value(case, &router).await;
        let expected_publication = if row.cancelled {
            Some(PUBLISHED.to_string())
        } else {
            row.revised_publication.map(ToString::to_string)
        };
        assert_eq!(
            publishing, expected_publication,
            "case {case:?}: expected {EXPORT} to publish {expected_publication:?}, got \
             {publishing:?}"
        );
    }
}
